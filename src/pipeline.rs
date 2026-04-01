use crate::config::Config;
use crate::notify;
use crate::output;
use crate::rate_limit::{is_rate_limited, Backoff};
use crate::runner::{CommandRunner, RunError};
use crate::stage::{Stage, StageResult};
use crate::verifier::{Verifier, VerifyFeedback};

// ─── Pipeline definition ───────────────────────────────────────────

/// A pipeline is an ordered sequence of steps.
#[derive(Debug, Clone)]
pub struct Pipeline {
    pub steps: Vec<PipelineStep>,
}

/// A single step in a pipeline.
#[derive(Debug, Clone)]
pub enum PipelineStep {
    /// Run a stage once.
    Run(Stage),

    /// Run a worker stage, then verify. Loop on failure.
    VerifyLoop {
        worker: Stage,
        verifier: Verifier,
        max_rounds: u32,
    },
}

// ─── Pipeline outcomes ─────────────────────────────────────────────

/// Final outcome of running an entire pipeline.
#[derive(Debug)]
pub enum PipelineOutcome {
    Success,
    VerifyExhausted,
    StageFailed { exit_code: i32 },
}

impl PipelineOutcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Success => 0,
            Self::VerifyExhausted => 1,
            Self::StageFailed { exit_code } => *exit_code,
        }
    }
}

/// Outcome of a single verify loop.
#[derive(Debug)]
pub enum VerifyOutcome {
    Passed { round: u32, score: Option<u32> },
    ExhaustedRounds,
    StageFailed { exit_code: i32, round: u32 },
}

// ─── PipelineRunner ────────────────────────────────────────────────

/// Orchestrates pipeline execution with rate-limit retry and verification loops.
pub struct PipelineRunner<R: CommandRunner> {
    pub cmd: R,
    pub config: Config,
    pub base_session: String,
    pub extra_args: Vec<String>,
}

impl<R: CommandRunner> PipelineRunner<R> {
    /// Run an entire pipeline to completion.
    pub async fn run(&self, pipeline: &Pipeline) -> PipelineOutcome {
        for step in &pipeline.steps {
            match step {
                PipelineStep::Run(stage) => {
                    if let Err(e) = self.run_stage(stage, false).await {
                        return PipelineOutcome::StageFailed {
                            exit_code: e.exit_code(),
                        };
                    }
                }
                PipelineStep::VerifyLoop {
                    worker,
                    verifier,
                    max_rounds,
                } => match self.run_verify_loop(worker, verifier, *max_rounds).await {
                    VerifyOutcome::Passed { .. } => {}
                    VerifyOutcome::ExhaustedRounds => return PipelineOutcome::VerifyExhausted,
                    VerifyOutcome::StageFailed { exit_code, .. } => {
                        return PipelineOutcome::StageFailed { exit_code };
                    }
                },
            }
        }
        PipelineOutcome::Success
    }

    // ─── Stage execution ───────────────────────────────────────────

    /// Execute a single stage. Claude stages get rate-limit retry.
    pub async fn run_stage(&self, stage: &Stage, is_resume: bool) -> Result<StageResult, RunError> {
        match stage {
            Stage::Claude {
                prompt,
                capture_output,
                ..
            } => {
                if *capture_output {
                    self.run_claude_capturing_with_retry(stage, prompt, is_resume)
                        .await
                } else {
                    self.run_claude_with_retry(prompt, is_resume).await
                }
            }
            Stage::Shell { command, .. } => {
                let result = self.cmd.run_shell(command).await.map_err(RunError::Io)?;
                Ok(StageResult {
                    exit_code: result.exit_code,
                    stdout: result.stdout,
                    stderr: result.stderr,
                })
            }
        }
    }

    /// Build the argument list for a Claude invocation.
    fn build_claude_args(&self, stage: &Stage, prompt: &str, is_resume: bool) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
        ];

        if is_resume {
            args.push("--continue".to_string());
        }

        args.push(prompt.to_string());

        // Session name: base + suffix
        let session_name = self.session_name_for(stage);
        if !session_name.is_empty() {
            args.push("--name".to_string());
            args.push(session_name);
        }

        // Model override from stage
        if let Stage::Claude { model: Some(m), .. } = stage {
            args.push("--model".to_string());
            args.push(m.clone());
        }

        // Stage-specific extra args
        if let Stage::Claude { extra_args, .. } = stage {
            args.extend(extra_args.iter().cloned());
        }

        // Global extra args (from CLI passthrough)
        args.extend(self.extra_args.iter().cloned());

        args
    }

    /// Compute the session name for a stage.
    fn session_name_for(&self, stage: &Stage) -> String {
        match stage {
            Stage::Claude { session_suffix, .. } => {
                if session_suffix.is_empty() {
                    self.base_session.clone()
                } else {
                    format!("{}{}", self.base_session, session_suffix)
                }
            }
            _ => self.base_session.clone(),
        }
    }

    /// Run Claude with automatic rate-limit retry and backoff.
    async fn run_claude_with_retry(
        &self,
        prompt: &str,
        is_resume: bool,
    ) -> Result<StageResult, RunError> {
        let worker_stage = Stage::claude_worker(prompt);
        let mut backoff = Backoff::new(self.config.retry_delay, self.config.retry_cap);
        let mut attempt: u32 = 0;
        let mut resume = is_resume;
        let mut current_prompt = prompt.to_string();

        loop {
            let args = self.build_claude_args(&worker_stage, &current_prompt, resume);
            let result = self.cmd.run_claude(&args).await.map_err(RunError::Io)?;

            if result.exit_code == 0 {
                return Ok(StageResult {
                    exit_code: 0,
                    stdout: result.stdout,
                    stderr: result.stderr,
                });
            }

            if is_rate_limited(result.exit_code, &result.stderr) {
                attempt += 1;

                if attempt > self.config.max_retries {
                    self.wait_for_cap_reset().await?;
                    attempt = 0;
                    backoff.reset();
                    resume = true;
                    current_prompt = "continue where you left off".to_string();
                    output::resuming(&self.base_session);
                    continue;
                }

                let delay = backoff.next_delay();
                output::rate_limited(attempt, self.config.max_retries, delay.as_secs());
                tokio::time::sleep(delay).await;
                resume = true;
                current_prompt = "continue where you left off".to_string();
                output::resuming(&self.base_session);
            } else {
                output::claude_error(result.exit_code);
                return Err(RunError::ClaudeFailed(result.exit_code));
            }
        }
    }

    /// Run a Claude stage with stdout captured (for verdict parsing).
    async fn run_claude_capturing_with_retry(
        &self,
        stage: &Stage,
        prompt: &str,
        is_resume: bool,
    ) -> Result<StageResult, RunError> {
        let mut backoff = Backoff::new(self.config.retry_delay, self.config.retry_cap);
        let mut attempt: u32 = 0;
        let mut resume = is_resume;
        let mut current_prompt = prompt.to_string();

        loop {
            let args = self.build_claude_args(stage, &current_prompt, resume);
            let result = self
                .cmd
                .run_claude_capturing(&args)
                .await
                .map_err(RunError::Io)?;

            if result.exit_code == 0 {
                return Ok(StageResult {
                    exit_code: 0,
                    stdout: result.stdout,
                    stderr: result.stderr,
                });
            }

            if is_rate_limited(result.exit_code, &result.stderr) {
                attempt += 1;

                if attempt > self.config.max_retries {
                    self.wait_for_cap_reset().await?;
                    attempt = 0;
                    backoff.reset();
                    resume = true;
                    current_prompt = "continue where you left off".to_string();
                    output::resuming(&self.session_name_for(stage));
                    continue;
                }

                let delay = backoff.next_delay();
                output::rate_limited(attempt, self.config.max_retries, delay.as_secs());
                tokio::time::sleep(delay).await;
                resume = true;
                current_prompt = "continue where you left off".to_string();
                output::resuming(&self.session_name_for(stage));
            } else {
                output::claude_error(result.exit_code);
                return Err(RunError::ClaudeFailed(result.exit_code));
            }
        }
    }

    /// Poll until the daily rate limit cap resets.
    async fn wait_for_cap_reset(&self) -> Result<(), RunError> {
        output::daily_cap_waiting(
            self.config.max_retries,
            self.config.daily_cap_poll.as_secs(),
            self.config.daily_cap_timeout.as_secs(),
        );
        notify::notify(
            &format!(
                "Likely daily cap — waiting for reset: {}",
                self.base_session
            ),
            self.config.notify,
        );

        let mut waited = std::time::Duration::ZERO;

        while waited < self.config.daily_cap_timeout {
            tokio::time::sleep(self.config.daily_cap_poll).await;
            waited += self.config.daily_cap_poll;

            output::daily_cap_probe(waited.as_secs());

            let probe_args = vec![
                "-p".to_string(),
                "--max-turns".to_string(),
                "1".to_string(),
                "ping".to_string(),
            ];
            let result = self
                .cmd
                .run_claude(&probe_args)
                .await
                .map_err(RunError::Io)?;

            if result.exit_code == 0 {
                output::daily_cap_lifted();
                notify::notify(
                    &format!("Rate limit lifted — resuming: {}", self.base_session),
                    self.config.notify,
                );
                return Ok(());
            }

            if !is_rate_limited(result.exit_code, &result.stderr) {
                return Err(RunError::ClaudeFailed(result.exit_code));
            }
        }

        notify::notify(
            &format!("Timed out waiting for cap reset: {}", self.base_session),
            self.config.notify,
        );
        Err(RunError::DailyCapTimeout)
    }

    // ─── Verify loop ───────────────────────────────────────────────

    /// Run the generic verify-fix loop.
    async fn run_verify_loop(
        &self,
        worker: &Stage,
        verifier: &Verifier,
        max_rounds: u32,
    ) -> VerifyOutcome {
        // Initial worker run
        if let Err(e) = self.run_stage(worker, false).await {
            return VerifyOutcome::StageFailed {
                exit_code: e.exit_code(),
                round: 0,
            };
        }

        for round in 1..=max_rounds {
            // Run verifier
            let feedback = match self.run_verifier(verifier).await {
                Ok(fb) => fb,
                Err(e) => {
                    return VerifyOutcome::StageFailed {
                        exit_code: e.exit_code(),
                        round,
                    };
                }
            };

            if feedback.passed {
                output::verify_passed();
                return VerifyOutcome::Passed {
                    round,
                    score: feedback.score,
                };
            }

            if round == max_rounds {
                break;
            }

            // Build fix prompt and resume worker
            let fix_prompt = self.build_fix_prompt(verifier, &feedback);
            output::verify_failed(0);

            if let Err(e) = self.run_claude_with_retry(&fix_prompt, true).await {
                return VerifyOutcome::StageFailed {
                    exit_code: e.exit_code(),
                    round,
                };
            }
        }

        // One final verify after the last fix attempt
        if let Ok(feedback) = self.run_verifier(verifier).await {
            if feedback.passed {
                output::verify_passed();
                return VerifyOutcome::Passed {
                    round: max_rounds,
                    score: feedback.score,
                };
            }
        }

        output::verify_exhausted(max_rounds);
        notify::notify(
            &format!(
                "Gave up after {} verify rounds: {}",
                max_rounds, self.base_session
            ),
            self.config.notify,
        );
        VerifyOutcome::ExhaustedRounds
    }

    /// Run a verifier and produce feedback.
    fn run_verifier<'a>(
        &'a self,
        verifier: &'a Verifier,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<VerifyFeedback, RunError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match verifier {
                Verifier::Shell { command } => {
                    output::verify_round(0, 0, command);
                    let result = self.cmd.run_shell(command).await.map_err(RunError::Io)?;
                    let combined = format!("{}{}", result.stdout, result.stderr);
                    Ok(VerifyFeedback::from_shell(result.exit_code, &combined))
                }
                Verifier::Claude {
                    stage,
                    verdict_parser,
                } => {
                    let result = self.run_stage(stage, false).await?;
                    let feedback =
                        crate::verdict::parse_to_feedback(&result.stdout, verdict_parser);
                    Ok(feedback)
                }
                Verifier::Chain(verifiers) => {
                    for v in verifiers {
                        let feedback = self.run_verifier(v).await?;
                        if !feedback.passed {
                            return Ok(feedback);
                        }
                    }
                    Ok(VerifyFeedback {
                        passed: true,
                        ..Default::default()
                    })
                }
            }
        })
    }

    /// Build a fix prompt from verifier feedback.
    fn build_fix_prompt(&self, verifier: &Verifier, feedback: &VerifyFeedback) -> String {
        match verifier {
            Verifier::Shell { command } => {
                format!(
                    "The verification command `{command}` failed (exit code 0). \
                     Fix the issues and try again. Here is the output:\n\n```\n{}\n```",
                    feedback.summary
                )
            }
            Verifier::Claude { .. } => crate::prompts::build_av_fix_prompt(feedback),
            Verifier::Chain(verifiers) => {
                // Find which verifier failed and use its fix prompt style
                if let Some(v) = verifiers.last() {
                    self.build_fix_prompt(v, feedback)
                } else {
                    format!("Fix the issues:\n{}", feedback.summary)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RunResult;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // ─── Mock runner ───────────────────────────────────────────────

    struct MockRunner {
        claude_results: std::sync::Mutex<Vec<RunResult>>,
        shell_results: std::sync::Mutex<Vec<RunResult>>,
        claude_calls: AtomicU32,
        shell_calls: AtomicU32,
    }

    impl MockRunner {
        fn new(claude: Vec<RunResult>, shell: Vec<RunResult>) -> Arc<Self> {
            Arc::new(Self {
                claude_results: std::sync::Mutex::new(claude),
                shell_results: std::sync::Mutex::new(shell),
                claude_calls: AtomicU32::new(0),
                shell_calls: AtomicU32::new(0),
            })
        }

        fn claude_calls(&self) -> u32 {
            self.claude_calls.load(Ordering::SeqCst)
        }

        fn shell_calls(&self) -> u32 {
            self.shell_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CommandRunner for Arc<MockRunner> {
        async fn run_claude(&self, _args: &[String]) -> std::io::Result<RunResult> {
            self.claude_calls.fetch_add(1, Ordering::SeqCst);
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

        async fn run_claude_capturing(&self, args: &[String]) -> std::io::Result<RunResult> {
            self.run_claude(args).await
        }

        async fn run_shell(&self, _cmd: &str) -> std::io::Result<RunResult> {
            self.shell_calls.fetch_add(1, Ordering::SeqCst);
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
            av_threshold: 95,
            av_rounds: 3,
            av_model: None,
        }
    }

    fn test_runner(mock: Arc<MockRunner>) -> PipelineRunner<Arc<MockRunner>> {
        PipelineRunner {
            cmd: mock,
            config: test_config(),
            base_session: "test".into(),
            extra_args: vec![],
        }
    }

    // ─── Pipeline tests ────────────────────────────────────────────

    #[tokio::test]
    async fn simple_run_stage() {
        let mock = MockRunner::new(
            vec![RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }],
            vec![],
        );
        let runner = test_runner(mock.clone());
        let pipeline = Pipeline {
            steps: vec![PipelineStep::Run(Stage::claude_worker("do something"))],
        };
        let outcome = runner.run(&pipeline).await;
        assert!(matches!(outcome, PipelineOutcome::Success));
        assert_eq!(mock.claude_calls(), 1);
    }

    #[tokio::test]
    async fn verify_loop_passes_first_round() {
        let mock = MockRunner::new(
            // Worker succeeds
            vec![RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }],
            // Verify passes
            vec![RunResult {
                exit_code: 0,
                stdout: "all tests passed".into(),
                stderr: String::new(),
            }],
        );
        let runner = test_runner(mock.clone());
        let pipeline = Pipeline {
            steps: vec![PipelineStep::VerifyLoop {
                worker: Stage::claude_worker("implement feature"),
                verifier: Verifier::Shell {
                    command: "make test".into(),
                },
                max_rounds: 3,
            }],
        };
        let outcome = runner.run(&pipeline).await;
        assert!(matches!(outcome, PipelineOutcome::Success));
        assert_eq!(mock.claude_calls(), 1); // worker run
        assert_eq!(mock.shell_calls(), 1); // verify run
    }

    #[tokio::test]
    async fn verify_loop_fails_then_passes() {
        let mock = MockRunner::new(
            vec![
                // Initial worker run
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Fix attempt (resume)
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            ],
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
        let runner = test_runner(mock.clone());
        let pipeline = Pipeline {
            steps: vec![PipelineStep::VerifyLoop {
                worker: Stage::claude_worker("implement feature"),
                verifier: Verifier::Shell {
                    command: "make test".into(),
                },
                max_rounds: 3,
            }],
        };
        let outcome = runner.run(&pipeline).await;
        assert!(matches!(outcome, PipelineOutcome::Success));
    }

    #[tokio::test]
    async fn verify_loop_exhausts_rounds() {
        let mock = MockRunner::new(
            vec![
                // Initial worker
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Fix attempts
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
            // All verifications fail
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
                // Final verify also fails
                RunResult {
                    exit_code: 1,
                    stdout: "fail".into(),
                    stderr: String::new(),
                },
            ],
        );
        let runner = test_runner(mock);
        let pipeline = Pipeline {
            steps: vec![PipelineStep::VerifyLoop {
                worker: Stage::claude_worker("implement feature"),
                verifier: Verifier::Shell {
                    command: "make test".into(),
                },
                max_rounds: 3,
            }],
        };
        let outcome = runner.run(&pipeline).await;
        assert!(matches!(outcome, PipelineOutcome::VerifyExhausted));
    }

    #[tokio::test]
    async fn retries_on_rate_limit() {
        let mock = MockRunner::new(
            vec![
                RunResult {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: "rate limit exceeded".into(),
                },
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            ],
            vec![],
        );
        let runner = test_runner(mock.clone());
        let pipeline = Pipeline {
            steps: vec![PipelineStep::Run(Stage::claude_worker("do something"))],
        };
        let outcome = runner.run(&pipeline).await;
        assert!(matches!(outcome, PipelineOutcome::Success));
        assert_eq!(mock.claude_calls(), 2);
    }

    #[tokio::test]
    async fn non_rate_limit_error_fails_immediately() {
        let mock = MockRunner::new(
            vec![RunResult {
                exit_code: 2,
                stdout: String::new(),
                stderr: "unknown error".into(),
            }],
            vec![],
        );
        let runner = test_runner(mock.clone());
        let pipeline = Pipeline {
            steps: vec![PipelineStep::Run(Stage::claude_worker("do something"))],
        };
        let outcome = runner.run(&pipeline).await;
        match outcome {
            PipelineOutcome::StageFailed { exit_code } => assert_eq!(exit_code, 2),
            other => panic!("expected StageFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_args_includes_session_name() {
        let mock = MockRunner::new(vec![], vec![]);
        let runner = PipelineRunner {
            cmd: mock,
            config: test_config(),
            base_session: "my-session".into(),
            extra_args: vec!["--max-turns".into(), "50".into()],
        };
        let stage = Stage::claude_worker("do something");
        let args = runner.build_claude_args(&stage, "do something", false);
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"my-session".to_string()));
        assert!(args.contains(&"--max-turns".to_string()));
        assert!(args.contains(&"50".to_string()));
    }

    #[tokio::test]
    async fn build_args_resume() {
        let mock = MockRunner::new(vec![], vec![]);
        let runner = test_runner(mock);
        let stage = Stage::claude_worker("continue");
        let args = runner.build_claude_args(&stage, "continue", true);
        assert!(args.contains(&"--continue".to_string()));
    }

    #[tokio::test]
    async fn session_name_with_suffix() {
        let mock = MockRunner::new(vec![], vec![]);
        let runner = PipelineRunner {
            cmd: mock,
            config: test_config(),
            base_session: "impl-spec".into(),
            extra_args: vec![],
        };
        let stage = Stage::claude_reviewer("review", "-av-1", None);
        assert_eq!(runner.session_name_for(&stage), "impl-spec-av-1");
    }

    #[tokio::test]
    async fn daily_cap_timeout() {
        let mut results = Vec::new();
        // max_retries + 1 rate-limited responses
        for _ in 0..4 {
            results.push(RunResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: "rate limit exceeded".into(),
            });
        }
        // Daily cap probes also rate-limited
        for _ in 0..20 {
            results.push(RunResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: "rate limit exceeded".into(),
            });
        }

        let mock = MockRunner::new(results, vec![]);
        let runner = test_runner(mock);
        let pipeline = Pipeline {
            steps: vec![PipelineStep::Run(Stage::claude_worker("do something"))],
        };
        let outcome = runner.run(&pipeline).await;
        assert!(matches!(outcome, PipelineOutcome::StageFailed { .. }));
    }
}
