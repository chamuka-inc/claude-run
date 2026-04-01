use claude_run_lib::config::Config;
use claude_run_lib::output;
use claude_run_lib::runner::{CommandRunner, TokioCommandRunner};
use claude_run_lib::verifier::VerifyFeedback;

const HELP: &str = "\
Usage: claude-run verify [OPTIONS]

Generic verify-fix loop: run a worker command, then a check command.
If the check fails, re-invoke the worker with --continue and the error
output. Repeats up to --max-rounds.

Options:
  --worker CMD       The worker command to run (required)
  --check CMD        The verification command (required)
  --max-rounds N     Max fix attempts (default: 5 or CLAUDE_VERIFY_MAX)

The worker is initially run as-is. On subsequent rounds, it is re-run
with the check's error output appended as an argument.

Examples:
  claude-run verify \\
    --worker 'claude-run retry --name feat \"implement login\"' \\
    --check 'make test'

  claude-run verify \\
    --worker 'claude-run retry --name feat' \\
    --check 'make ci && claude-run review --spec spec.md'";

pub async fn run(args: Vec<String>) -> i32 {
    let mut worker: Option<String> = None;
    let mut check: Option<String> = None;
    let mut max_rounds: Option<u32> = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return 0;
            }
            "--worker" => worker = iter.next(),
            "--check" => check = iter.next(),
            "--max-rounds" => max_rounds = iter.next().and_then(|v| v.parse().ok()),
            _ => {
                eprintln!("Unknown flag: {arg}");
                return 1;
            }
        }
    }

    let Some(worker_cmd) = worker else {
        eprintln!(
            "Error: --worker is required.\nUsage: claude-run verify --worker CMD --check CMD"
        );
        return 1;
    };
    let Some(check_cmd) = check else {
        eprintln!("Error: --check is required.\nUsage: claude-run verify --worker CMD --check CMD");
        return 1;
    };

    let config = Config::from_env();
    let max = max_rounds.unwrap_or(config.verify_max);
    let cmd = TokioCommandRunner;

    // Initial worker run
    output::banner("verify-loop", Some(&check_cmd), None, None);

    let result = cmd.run_shell(&worker_cmd).await;
    match result {
        Ok(r) if r.exit_code != 0 => {
            output::claude_error(r.exit_code);
            return r.exit_code;
        }
        Err(e) => {
            eprintln!("Failed to run worker: {e}");
            return 1;
        }
        _ => {}
    }

    for round in 1..=max {
        output::verify_round(round, max, &check_cmd);

        let check_result = match cmd.run_shell(&check_cmd).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to run check command: {e}");
                return 1;
            }
        };

        if check_result.exit_code == 0 {
            output::verify_passed();
            return 0;
        }

        if round == max {
            break;
        }

        output::verify_failed(check_result.exit_code);

        // Build feedback and re-run worker with --continue + error output
        let combined = format!("{}{}", check_result.stdout, check_result.stderr);
        let feedback = VerifyFeedback::from_shell(check_result.exit_code, &combined);

        let fix_prompt = format!(
            "The verification command `{}` failed (exit code {}). \
             Fix the issues and try again. Here is the output:\n\n```\n{}\n```",
            check_cmd, check_result.exit_code, feedback.summary
        );

        // Re-run worker with the fix prompt appended
        let resume_cmd = format!("{worker_cmd} --continue \"{fix_prompt}\"");
        match cmd.run_shell(&resume_cmd).await {
            Ok(r) if r.exit_code != 0 => {
                output::claude_error(r.exit_code);
                return r.exit_code;
            }
            Err(e) => {
                eprintln!("Worker failed during fix: {e}");
                return 1;
            }
            _ => {}
        }
    }

    output::verify_exhausted(max);
    1
}
