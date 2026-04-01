use claude_run_lib::config::Config;
use claude_run_lib::notify;
use claude_run_lib::output;
use claude_run_lib::pipeline::{PipelineOutcome, PipelineRunner};
use claude_run_lib::runner::TokioCommandRunner;
use claude_run_lib::yaml_pipeline;

const HELP: &str = "\
Usage: claude-run pipeline [OPTIONS] FILE

Load and execute a multi-stage pipeline from a YAML file.

Options:
  --name NAME        Session name (default: derived from filename)

All other flags pass through to Claude stages.

Examples:
  claude-run pipeline pipeline.yaml
  claude-run pipeline --name my-run pipeline.yaml";

pub async fn run(args: Vec<String>) -> i32 {
    let mut name: Option<String> = None;
    let mut file: Option<String> = None;
    let mut extra: Vec<String> = Vec::new();

    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return 0;
            }
            "--name" => name = iter.next(),
            _ if arg.starts_with('-') => {
                extra.push(arg);
                if let Some(next) = iter.peek() {
                    if !next.starts_with('-') {
                        extra.push(iter.next().unwrap());
                    }
                }
            }
            _ => {
                if file.is_none() {
                    file = Some(arg);
                } else {
                    eprintln!("Error: unexpected argument '{arg}'");
                    return 1;
                }
            }
        }
    }

    let Some(path) = file else {
        eprintln!("Error: YAML file path required.\nUsage: claude-run pipeline FILE");
        return 1;
    };

    let pipeline = match yaml_pipeline::load_pipeline(std::path::Path::new(&path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error loading pipeline from {path}:\n{e}");
            return 1;
        }
    };

    let config = Config::from_env();

    let session_name = name.unwrap_or_else(|| {
        std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("pipeline")
            .to_string()
    });

    output::banner(&session_name, None, None, Some(&path));

    let runner = PipelineRunner {
        config: config.clone(),
        base_session: session_name.clone(),
        extra_args: extra,
        cmd: TokioCommandRunner,
    };

    let outcome = runner.run(&pipeline).await;

    match &outcome {
        PipelineOutcome::Success => {
            output::done(&session_name);
            notify::notify(&format!("Pipeline complete: {session_name}"), config.notify);
        }
        PipelineOutcome::VerifyExhausted => {
            notify::notify(
                &format!("Verification exhausted: {session_name}"),
                config.notify,
            );
        }
        PipelineOutcome::StageFailed { .. } => {
            notify::notify(
                &format!(
                    "Pipeline failed (exit {}): {session_name}",
                    outcome.exit_code()
                ),
                config.notify,
            );
        }
    }

    outcome.exit_code()
}
