use crate::notify;
use crate::output;
use crate::runner::{ClaudeRunner, CommandRunner, RunError};

/// Outcome of the verify loop.
#[derive(Debug)]
pub enum VerifyOutcome {
    Passed { round: u32 },
    ExhaustedRounds,
    ClaudeFailed { exit_code: i32, round: u32 },
}

/// Run the Ralph Wiggum verify-fix loop.
///
/// 1. Run the verify command
/// 2. If it passes, we're done
/// 3. If it fails, send Claude back in with the failure output to fix
/// 4. Repeat up to `max_rounds`
pub async fn run_verify_loop<R: CommandRunner>(
    runner: &ClaudeRunner<R>,
    verify_cmd: &str,
) -> VerifyOutcome {
    let max = runner.config.verify_max;

    for round in 1..=max {
        output::verify_round(round, max, verify_cmd);

        let verify_result = match runner.cmd.run_shell(verify_cmd).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to run verify command: {e}");
                return VerifyOutcome::ClaudeFailed {
                    exit_code: 1,
                    round,
                };
            }
        };

        if verify_result.exit_code == 0 {
            output::verify_passed();
            return VerifyOutcome::Passed { round };
        }

        output::verify_failed(verify_result.exit_code);

        // Take last 200 lines of combined output to avoid token overflow
        let combined = format!("{}{}", verify_result.stdout, verify_result.stderr);
        let tail: String = combined
            .lines()
            .rev()
            .take(200)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");

        let fix_prompt = format!(
            "The verification command `{verify_cmd}` failed (exit code {}). \
             Fix the issues and try again. Here is the output:\n\n```\n{tail}\n```",
            verify_result.exit_code
        );

        match runner.run_with_retry(&fix_prompt, true).await {
            Ok(()) => {}
            Err(RunError::ClaudeFailed(code)) => {
                return VerifyOutcome::ClaudeFailed {
                    exit_code: code,
                    round,
                };
            }
            Err(e) => {
                eprintln!("Claude failed during fix attempt: {e}");
                return VerifyOutcome::ClaudeFailed {
                    exit_code: e.exit_code(),
                    round,
                };
            }
        }
    }

    // One final verify after the last fix
    output::verify_round(max, max, verify_cmd);
    if let Ok(result) = runner.cmd.run_shell(verify_cmd).await {
        if result.exit_code == 0 {
            output::verify_passed();
            return VerifyOutcome::Passed { round: max };
        }
    }

    output::verify_exhausted(max);
    notify::notify(
        &format!("Gave up after {max} verify rounds: {}", runner.session_name),
        runner.config.notify,
    );
    VerifyOutcome::ExhaustedRounds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runner::RunResult;
    use std::sync::Arc;

    struct SequencedRunner {
        claude_results: std::sync::Mutex<Vec<RunResult>>,
        shell_results: std::sync::Mutex<Vec<RunResult>>,
    }

    impl SequencedRunner {
        fn new(claude: Vec<RunResult>, shell: Vec<RunResult>) -> Arc<Self> {
            Arc::new(Self {
                claude_results: std::sync::Mutex::new(claude),
                shell_results: std::sync::Mutex::new(shell),
            })
        }
    }

    #[async_trait::async_trait]
    impl CommandRunner for Arc<SequencedRunner> {
        async fn run_claude(&self, _args: &[String]) -> std::io::Result<RunResult> {
            let mut results = self.claude_results.lock().unwrap();
            if results.is_empty() {
                Ok(RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            } else {
                Ok(results.remove(0))
            }
        }

        async fn run_shell(&self, _cmd: &str) -> std::io::Result<RunResult> {
            let mut results = self.shell_results.lock().unwrap();
            if results.is_empty() {
                Ok(RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            } else {
                Ok(results.remove(0))
            }
        }
    }

    fn test_config() -> Config {
        Config {
            max_retries: 3,
            retry_delay: std::time::Duration::from_millis(1),
            retry_cap: std::time::Duration::from_millis(10),
            notify: false,
            verify_max: 3,
            daily_cap_poll: std::time::Duration::from_millis(1),
            daily_cap_timeout: std::time::Duration::from_millis(10),
        }
    }

    #[tokio::test]
    async fn verify_passes_first_round() {
        let mock = SequencedRunner::new(
            vec![],
            vec![RunResult {
                exit_code: 0,
                stdout: "all tests passed".into(),
                stderr: String::new(),
            }],
        );
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };
        match run_verify_loop(&runner, "make test").await {
            VerifyOutcome::Passed { round } => assert_eq!(round, 1),
            other => panic!("expected Passed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_fails_then_passes() {
        let mock = SequencedRunner::new(
            // Claude fix attempt succeeds
            vec![RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }],
            vec![
                // Round 1: verify fails
                RunResult {
                    exit_code: 1,
                    stdout: "test failed".into(),
                    stderr: String::new(),
                },
                // Round 2: verify passes
                RunResult {
                    exit_code: 0,
                    stdout: "all tests passed".into(),
                    stderr: String::new(),
                },
            ],
        );
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };
        match run_verify_loop(&runner, "make test").await {
            VerifyOutcome::Passed { round } => assert_eq!(round, 2),
            other => panic!("expected Passed round 2, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_exhausts_all_rounds() {
        let mock = SequencedRunner::new(
            // Claude "fixes" succeed each time
            vec![
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            ],
            // But verify always fails
            vec![
                RunResult {
                    exit_code: 1,
                    stdout: "fail".into(),
                    stderr: String::new(),
                },
                RunResult {
                    exit_code: 1,
                    stdout: "fail".into(),
                    stderr: String::new(),
                },
                RunResult {
                    exit_code: 1,
                    stdout: "fail".into(),
                    stderr: String::new(),
                },
                // Final verify after last fix also fails
                RunResult {
                    exit_code: 1,
                    stdout: "fail".into(),
                    stderr: String::new(),
                },
            ],
        );
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };
        match run_verify_loop(&runner, "make test").await {
            VerifyOutcome::ExhaustedRounds => {} // expected
            other => panic!("expected ExhaustedRounds, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_fails_during_fix() {
        let mock = SequencedRunner::new(
            // Claude fix fails
            vec![RunResult {
                exit_code: 2,
                stdout: String::new(),
                stderr: "internal error".into(),
            }],
            // Verify fails
            vec![RunResult {
                exit_code: 1,
                stdout: "test failed".into(),
                stderr: String::new(),
            }],
        );
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };
        match run_verify_loop(&runner, "make test").await {
            VerifyOutcome::ClaudeFailed { exit_code, round } => {
                assert_eq!(exit_code, 2);
                assert_eq!(round, 1);
            }
            other => panic!("expected ClaudeFailed, got {other:?}"),
        }
    }
}
