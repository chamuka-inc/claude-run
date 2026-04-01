use async_trait::async_trait;

use crate::config::Config;
use crate::notify;
use crate::output;
use crate::runner::{ClaudeRunner, CommandRunner, RunError};

// ── Core abstractions ──────────────────────────────────────────

/// Result of a single verification check.
#[derive(Debug)]
pub enum CheckResult {
    /// The check passed.
    Pass,
    /// The check failed. `feedback` is sent to the fixer.
    Fail { feedback: String },
    /// The check itself errored (e.g., couldn't spawn process).
    Error { exit_code: i32 },
}

/// Outcome of the full verify-fix loop.
#[derive(Debug)]
pub enum VerifyOutcome {
    Passed { round: u32 },
    ExhaustedRounds,
    FixerFailed { exit_code: i32, round: u32 },
}

/// A verifier is anything that can check work and produce feedback.
/// Shell commands, spec reviews, linters, type checkers — all verifiers.
#[async_trait]
pub trait Verifier: Send + Sync {
    /// Human-readable name for output (e.g., "make test", "spec review").
    fn name(&self) -> &str;

    /// Run the check for the given round. Returns pass/fail with feedback.
    async fn check(&self, round: u32) -> CheckResult;
}

// ── The universal verify-fix loop ──────────────────────────────

/// Run the check-fix loop: check → fail? → fix → recheck → loop.
///
/// This is the single loop that drives ALL verification in claude-run.
/// Both `--verify "make test"` (shell) and pipeline review (spec review)
/// go through this same loop. The only difference is the `Verifier` impl.
pub async fn run_verify_loop<R: CommandRunner, V: Verifier>(
    verifier: &V,
    fixer: &ClaudeRunner<R>,
    max_rounds: u32,
) -> VerifyOutcome {
    for round in 1..=max_rounds {
        output::verifier_round(verifier.name(), round, max_rounds);

        match verifier.check(round).await {
            CheckResult::Pass => {
                output::verifier_passed(verifier.name());
                return VerifyOutcome::Passed { round };
            }
            CheckResult::Error { exit_code } => {
                return VerifyOutcome::FixerFailed { exit_code, round };
            }
            CheckResult::Fail { feedback } => {
                output::verifier_failed(verifier.name());

                // Send the fixer back in with the feedback
                let fix_prompt = format!(
                    "The verifier `{}` failed. \
                     Fix the issues and try again. Here is the feedback:\n\n```\n{}\n```",
                    verifier.name(),
                    tail_lines(&feedback, 200),
                );

                match fixer.run_with_retry(&fix_prompt, true).await {
                    Ok(()) => {}
                    Err(RunError::ClaudeFailed(code)) => {
                        return VerifyOutcome::FixerFailed {
                            exit_code: code,
                            round,
                        };
                    }
                    Err(e) => {
                        eprintln!("Fixer failed during fix attempt: {e}");
                        return VerifyOutcome::FixerFailed {
                            exit_code: e.exit_code(),
                            round,
                        };
                    }
                }
            }
        }
    }

    // One final check after the last fix
    output::verifier_round(verifier.name(), max_rounds, max_rounds);
    if let CheckResult::Pass = verifier.check(max_rounds).await {
        output::verifier_passed(verifier.name());
        return VerifyOutcome::Passed { round: max_rounds };
    }

    output::verifier_exhausted(verifier.name(), max_rounds);
    notify::notify(
        &format!(
            "Gave up after {max_rounds} rounds ({}): {}",
            verifier.name(),
            fixer.session_name
        ),
        fixer.config.notify,
    );
    VerifyOutcome::ExhaustedRounds
}

// ── ShellVerifier ──────────────────────────────────────────────

/// Verifier that runs a shell command. Exit 0 = pass, anything else = fail.
/// Used for `--verify "make test"` and deterministic CI checks.
pub struct ShellVerifier<R: CommandRunner> {
    cmd: String,
    runner: R,
}

impl<R: CommandRunner> ShellVerifier<R> {
    pub fn new(cmd: &str, runner: R) -> Self {
        Self {
            cmd: cmd.to_string(),
            runner,
        }
    }
}

#[async_trait]
impl<R: CommandRunner> Verifier for ShellVerifier<R> {
    fn name(&self) -> &str {
        &self.cmd
    }

    async fn check(&self, _round: u32) -> CheckResult {
        let result = match self.runner.run_shell(&self.cmd).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to run verify command: {e}");
                return CheckResult::Error { exit_code: 1 };
            }
        };

        if result.exit_code == 0 {
            CheckResult::Pass
        } else {
            let combined = format!("{}{}", result.stdout, result.stderr);
            CheckResult::Fail {
                feedback: format!(
                    "Command `{}` failed (exit code {}):\n{}",
                    self.cmd, result.exit_code, combined
                ),
            }
        }
    }
}

// ── SpecReviewVerifier ─────────────────────────────────────────

/// Verifier that spawns an independent Claude instance to review
/// the implementation against a spec. Each round gets a fresh session
/// to avoid context contamination.
pub struct SpecReviewVerifier<R: CommandRunner> {
    spec_path: String,
    base_session: String,
    config: Config,
    extra_args: Vec<String>,
    runner: R,
}

impl<R: CommandRunner> SpecReviewVerifier<R> {
    pub fn new(
        spec_path: &str,
        base_session: &str,
        config: Config,
        extra_args: Vec<String>,
        runner: R,
    ) -> Self {
        Self {
            spec_path: spec_path.to_string(),
            base_session: base_session.to_string(),
            config,
            extra_args,
            runner,
        }
    }
}

#[async_trait]
impl<R: CommandRunner + Clone> Verifier for SpecReviewVerifier<R> {
    fn name(&self) -> &str {
        "spec review"
    }

    async fn check(&self, round: u32) -> CheckResult {
        let review_prompt = format!(
            "You are an independent code reviewer. Read the specification at \
             `{spec_path}` and review the current implementation against it.\n\
             \n\
             For each requirement in the spec:\n\
             1. Check if it is fully implemented\n\
             2. Check if the acceptance criteria are met\n\
             3. Check if edge cases are handled\n\
             \n\
             If EVERYTHING in the spec is correctly implemented, output exactly:\n\
             PIPELINE_VERDICT: PASS\n\
             \n\
             If there are ANY issues, output exactly:\n\
             PIPELINE_VERDICT: FAIL\n\
             followed by a numbered list of specific issues that need fixing.\n\
             \n\
             Be thorough but fair. Only flag genuine spec violations, not style preferences.",
            spec_path = self.spec_path,
        );

        // Fresh session per round — no context contamination
        let review_runner = ClaudeRunner {
            config: self.config.clone(),
            session_name: format!("{}-review-r{}", self.base_session, round),
            extra_args: self.extra_args.clone(),
            cmd: self.runner.clone(),
        };

        let args = review_runner.build_args_with_output_format(&review_prompt, false);
        let result = match review_runner.cmd.run_claude(&args).await {
            Ok(r) => r,
            Err(_) => return CheckResult::Error { exit_code: 1 },
        };

        if result.exit_code != 0 {
            return CheckResult::Error {
                exit_code: result.exit_code,
            };
        }

        let combined = format!("{}{}", result.stdout, result.stderr);
        if combined.contains("PIPELINE_VERDICT: PASS") {
            CheckResult::Pass
        } else {
            CheckResult::Fail { feedback: combined }
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────

/// Take the last N lines of a string to avoid token overflow.
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.to_string();
    }
    lines[lines.len() - n..].join("\n")
}

// ── Backward-compatible wrapper ────────────────────────────────

/// Convenience wrapper for standard (non-pipeline) mode.
/// The runner acts as both the fixer and the shell executor.
pub async fn run_shell_verify_loop<R: CommandRunner + Clone>(
    runner: &ClaudeRunner<R>,
    verify_cmd: &str,
) -> VerifyOutcome {
    let verifier = ShellVerifier::new(verify_cmd, runner.cmd.clone());
    run_verify_loop(&verifier, runner, runner.config.verify_max).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RunResult;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // ── Shared test mock ───────────────────────────────────────

    #[derive(Clone)]
    struct MockRunner {
        inner: Arc<MockRunnerInner>,
    }

    struct MockRunnerInner {
        claude_results: std::sync::Mutex<Vec<RunResult>>,
        shell_results: std::sync::Mutex<Vec<RunResult>>,
        claude_calls: AtomicU32,
        shell_calls: AtomicU32,
    }

    impl MockRunner {
        fn new(claude: Vec<RunResult>, shell: Vec<RunResult>) -> Self {
            Self {
                inner: Arc::new(MockRunnerInner {
                    claude_results: std::sync::Mutex::new(claude),
                    shell_results: std::sync::Mutex::new(shell),
                    claude_calls: AtomicU32::new(0),
                    shell_calls: AtomicU32::new(0),
                }),
            }
        }

        fn claude_calls(&self) -> u32 {
            self.inner.claude_calls.load(Ordering::SeqCst)
        }

        fn shell_calls(&self) -> u32 {
            self.inner.shell_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CommandRunner for MockRunner {
        async fn run_claude(&self, _args: &[String]) -> std::io::Result<RunResult> {
            self.inner.claude_calls.fetch_add(1, Ordering::SeqCst);
            let mut results = self.inner.claude_results.lock().unwrap();
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
            self.inner.shell_calls.fetch_add(1, Ordering::SeqCst);
            let mut results = self.inner.shell_results.lock().unwrap();
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
            pipeline_review_rounds: 2,
        }
    }

    fn test_fixer(mock: MockRunner) -> ClaudeRunner<MockRunner> {
        ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock,
        }
    }

    // ── ShellVerifier tests ────────────────────────────────────

    #[tokio::test]
    async fn shell_passes_first_round() {
        let mock = MockRunner::new(
            vec![], // no claude calls needed
            vec![RunResult {
                exit_code: 0,
                stdout: "all tests passed".into(),
                stderr: String::new(),
            }],
        );
        let verifier = ShellVerifier::new("make test", mock.clone());
        let fixer = test_fixer(mock);

        match run_verify_loop(&verifier, &fixer, 3).await {
            VerifyOutcome::Passed { round } => assert_eq!(round, 1),
            other => panic!("expected Passed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_fails_then_passes() {
        let mock = MockRunner::new(
            // Fixer succeeds
            vec![RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }],
            vec![
                // Round 1: shell fails
                RunResult {
                    exit_code: 1,
                    stdout: "test failed".into(),
                    stderr: String::new(),
                },
                // Round 2: shell passes
                RunResult {
                    exit_code: 0,
                    stdout: "all tests passed".into(),
                    stderr: String::new(),
                },
            ],
        );
        let verifier = ShellVerifier::new("make test", mock.clone());
        let fixer = test_fixer(mock);

        match run_verify_loop(&verifier, &fixer, 3).await {
            VerifyOutcome::Passed { round } => assert_eq!(round, 2),
            other => panic!("expected Passed round 2, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_exhausts_all_rounds() {
        let mock = MockRunner::new(
            // Fixer succeeds each time
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
            // Shell always fails
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
                // Final check also fails
                RunResult {
                    exit_code: 1,
                    stdout: "fail".into(),
                    stderr: String::new(),
                },
            ],
        );
        let verifier = ShellVerifier::new("make test", mock.clone());
        let fixer = test_fixer(mock);

        match run_verify_loop(&verifier, &fixer, 3).await {
            VerifyOutcome::ExhaustedRounds => {} // expected
            other => panic!("expected ExhaustedRounds, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fixer_fails_during_fix() {
        let mock = MockRunner::new(
            // Fixer fails
            vec![RunResult {
                exit_code: 2,
                stdout: String::new(),
                stderr: "internal error".into(),
            }],
            // Shell fails
            vec![RunResult {
                exit_code: 1,
                stdout: "test failed".into(),
                stderr: String::new(),
            }],
        );
        let verifier = ShellVerifier::new("make test", mock.clone());
        let fixer = test_fixer(mock);

        match run_verify_loop(&verifier, &fixer, 3).await {
            VerifyOutcome::FixerFailed { exit_code, round } => {
                assert_eq!(exit_code, 2);
                assert_eq!(round, 1);
            }
            other => panic!("expected FixerFailed, got {other:?}"),
        }
    }

    // ── SpecReviewVerifier tests ───────────────────────────────

    #[tokio::test]
    async fn review_passes_first_round() {
        let mock = MockRunner::new(
            vec![
                // Review returns PASS
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: PASS".into(),
                    stderr: String::new(),
                },
            ],
            vec![],
        );

        let verifier = SpecReviewVerifier::new(
            "spec.md",
            "test",
            test_config(),
            vec![],
            mock.clone(),
        );
        let fixer = test_fixer(mock);

        match run_verify_loop(&verifier, &fixer, 2).await {
            VerifyOutcome::Passed { round } => assert_eq!(round, 1),
            other => panic!("expected Passed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn review_fails_then_passes() {
        let mock = MockRunner::new(
            vec![
                // Review round 1: FAIL
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: FAIL\n1. Missing validation".into(),
                    stderr: String::new(),
                },
                // Fixer succeeds
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Review round 2: PASS
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: PASS".into(),
                    stderr: String::new(),
                },
            ],
            vec![],
        );

        let verifier = SpecReviewVerifier::new(
            "spec.md",
            "test",
            test_config(),
            vec![],
            mock.clone(),
        );
        let fixer = test_fixer(mock);

        match run_verify_loop(&verifier, &fixer, 3).await {
            VerifyOutcome::Passed { round } => assert_eq!(round, 2),
            other => panic!("expected Passed round 2, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn review_exhausts_rounds() {
        let mock = MockRunner::new(
            vec![
                // Review round 1: FAIL
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: FAIL\n1. Issue".into(),
                    stderr: String::new(),
                },
                // Fixer succeeds
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Review round 2: FAIL again
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: FAIL\n1. Still an issue".into(),
                    stderr: String::new(),
                },
                // Fixer succeeds
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Final check: still FAIL
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: FAIL\n1. Persistent issue".into(),
                    stderr: String::new(),
                },
            ],
            vec![],
        );

        let verifier = SpecReviewVerifier::new(
            "spec.md",
            "test",
            test_config(),
            vec![],
            mock.clone(),
        );
        let fixer = test_fixer(mock);

        match run_verify_loop(&verifier, &fixer, 2).await {
            VerifyOutcome::ExhaustedRounds => {} // expected
            other => panic!("expected ExhaustedRounds, got {other:?}"),
        }
    }

    // ── Backward compat wrapper test ───────────────────────────

    #[tokio::test]
    async fn shell_verify_loop_wrapper() {
        let mock = MockRunner::new(
            vec![],
            vec![RunResult {
                exit_code: 0,
                stdout: "pass".into(),
                stderr: String::new(),
            }],
        );
        let runner = test_fixer(mock);

        match run_shell_verify_loop(&runner, "make test").await {
            VerifyOutcome::Passed { round } => assert_eq!(round, 1),
            other => panic!("expected Passed, got {other:?}"),
        }
    }

    // ── Generic loop tests (verifier-agnostic) ─────────────────

    #[tokio::test]
    async fn verify_loop_calls_check_and_fixer_in_sequence() {
        let mock = MockRunner::new(
            // Fixer called once
            vec![RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }],
            vec![
                // Check 1: fail
                RunResult {
                    exit_code: 1,
                    stdout: "error on line 42".into(),
                    stderr: String::new(),
                },
                // Check 2: pass
                RunResult {
                    exit_code: 0,
                    stdout: "ok".into(),
                    stderr: String::new(),
                },
            ],
        );
        let verifier = ShellVerifier::new("cargo test", mock.clone());
        let fixer = test_fixer(mock.clone());

        let outcome = run_verify_loop(&verifier, &fixer, 5).await;
        assert!(matches!(outcome, VerifyOutcome::Passed { round: 2 }));
        // 1 fixer call (after first failure)
        assert_eq!(mock.claude_calls(), 1);
        // 2 shell calls (check, check)
        assert_eq!(mock.shell_calls(), 2);
    }

    // ── tail_lines helper tests ────────────────────────────────

    #[test]
    fn tail_lines_short() {
        assert_eq!(tail_lines("a\nb\nc", 10), "a\nb\nc");
    }

    #[test]
    fn tail_lines_truncates() {
        let input = (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let result = tail_lines(&input, 3);
        assert_eq!(result, "line 8\nline 9\nline 10");
    }
}
