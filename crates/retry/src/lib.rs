use claude_run_lib::config::Config;
use claude_run_lib::output;
use claude_run_lib::pipeline::{Pipeline, PipelineOutcome, PipelineRunner, PipelineStep};
use claude_run_lib::runner::TokioCommandRunner;
use claude_run_lib::slugify;
use claude_run_lib::stage::Stage;

const HELP: &str = "\
Usage: claude-run retry [OPTIONS] \"prompt\"

Wrap a Claude invocation with automatic rate-limit retry, exponential
backoff, and daily-cap polling.

Options:
  --name NAME        Session name (default: auto-generated from prompt)
  --resume [NAME]    Resume a session instead of starting new

All other flags pass through to claude (e.g. --max-turns 50, --model opus).

Examples:
  claude-run retry \"implement the login feature\"
  claude-run retry --name my-feat --model opus \"implement the feature\"
  claude-run retry --resume my-feat";

pub async fn run(args: Vec<String>) -> i32 {
    let mut prompt: Option<String> = None;
    let mut name: Option<String> = None;
    let mut resume: Option<String> = None;
    let mut extra: Vec<String> = Vec::new();

    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return 0;
            }
            "--name" => {
                name = iter.next();
            }
            "--resume" => {
                let target = iter.peek().filter(|n| !n.starts_with('-')).cloned();
                if target.is_some() {
                    iter.next();
                }
                resume = Some(target.unwrap_or_default());
            }
            _ if arg.starts_with('-') => {
                extra.push(arg);
                if let Some(next) = iter.peek() {
                    if !next.starts_with('-') {
                        extra.push(iter.next().unwrap());
                    }
                }
            }
            _ => {
                if prompt.is_none() {
                    prompt = Some(arg);
                } else {
                    extra.push(arg);
                }
            }
        }
    }

    let is_resume = resume.is_some();

    if !is_resume && prompt.is_none() {
        eprintln!("Error: No prompt provided.\nUsage: claude-run retry \"prompt\"");
        return 1;
    }

    let config = Config::from_env();

    let session_name = if is_resume {
        resume
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string()
    } else if let Some(n) = &name {
        n.clone()
    } else if let Some(p) = &prompt {
        slugify::slugify(p)
    } else {
        String::new()
    };

    output::banner(&session_name, None, None, None);

    let actual_prompt = if is_resume {
        "continue where you left off".to_string()
    } else {
        prompt.unwrap()
    };

    let worker = Stage::claude_worker(&actual_prompt);
    let pipeline = Pipeline {
        steps: vec![PipelineStep::Run(worker)],
    };

    let runner = PipelineRunner {
        config,
        base_session: session_name.clone(),
        extra_args: extra,
        cmd: TokioCommandRunner,
    };

    let outcome = runner.run(&pipeline).await;

    if matches!(&outcome, PipelineOutcome::Success) {
        output::done(&session_name);
    }

    outcome.exit_code()
}
