use claude_run_lib::config::Config;
use claude_run_lib::output;
use claude_run_lib::pipeline::{Pipeline, PipelineRunner, PipelineStep};
use claude_run_lib::prompts;
use claude_run_lib::runner::TokioCommandRunner;
use claude_run_lib::stage::Stage;
use claude_run_lib::verdict;

const HELP: &str = "\
Usage: claude-run review [OPTIONS]

Launch an independent Claude instance that reads a spec and scores the
current implementation 0-100. Outputs a structured verdict to stdout.

Options:
  --spec FILE        Path to the spec file (required)
  --threshold N      Minimum score to pass (default: 95)
  --model MODEL      Model for the reviewer (default: same as default)
  --prompt TEXT      Original task prompt (for context)

Exit code: 0 if score >= threshold, 1 if below.

Examples:
  claude-run review --spec spec.md
  claude-run review --spec spec.md --threshold 90 --model opus";

pub async fn run(args: Vec<String>) -> i32 {
    let mut spec: Option<String> = None;
    let mut threshold: Option<u32> = None;
    let mut model: Option<String> = None;
    let mut original_prompt: Option<String> = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return 0;
            }
            "--spec" => spec = iter.next(),
            "--threshold" => threshold = iter.next().and_then(|v| v.parse().ok()),
            "--model" => model = iter.next(),
            "--prompt" => original_prompt = iter.next(),
            _ => {
                eprintln!("Unknown flag: {arg}");
                return 1;
            }
        }
    }

    let Some(spec_file) = spec else {
        eprintln!("Error: --spec is required.\nUsage: claude-run review --spec FILE");
        return 1;
    };

    let config = Config::from_env();
    let threshold = threshold.unwrap_or(config.av_threshold);
    let original = original_prompt.as_deref().unwrap_or("");

    let review_prompt = prompts::build_review_prompt(original, Some(&spec_file));
    let reviewer = Stage::claude_reviewer(review_prompt, "-review", model);

    let pipeline = Pipeline {
        steps: vec![PipelineStep::Run(reviewer)],
    };

    let runner = PipelineRunner {
        config,
        base_session: "review".into(),
        extra_args: vec![],
        cmd: TokioCommandRunner,
    };

    // Run reviewer and capture output
    let result = match runner
        .run_stage(
            &pipeline
                .steps
                .first()
                .map(|s| match s {
                    PipelineStep::Run(stage) => stage.clone(),
                    _ => unreachable!(),
                })
                .unwrap(),
            false,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Reviewer failed: {e}");
            return e.exit_code();
        }
    };

    // Parse verdict
    let verdict = verdict::parse_verdict(&result.stdout);
    match verdict {
        verdict::ReviewVerdict::Scored(score) => {
            // Output structured result to stdout
            println!("SCORE: {}", score.score);
            println!("PASS: {}", score.score >= threshold);
            if !score.missing.is_empty() {
                println!("MISSING:");
                for item in &score.missing {
                    println!("- {item}");
                }
            }
            if !score.partial.is_empty() {
                println!("PARTIAL:");
                for item in &score.partial {
                    println!("- {item}");
                }
            }
            if !score.incorrect.is_empty() {
                println!("INCORRECT:");
                for item in &score.incorrect {
                    println!("- {item}");
                }
            }

            if score.score >= threshold {
                output::av_passed(score.score);
                0
            } else {
                output::av_exhausted(score.score, threshold, 1);
                1
            }
        }
        verdict::ReviewVerdict::NoVerdict => {
            output::av_no_verdict();
            1
        }
    }
}
