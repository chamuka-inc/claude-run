use crate::config::Config;
use crate::output;
use crate::runner::{ClaudeRunner, CommandRunner};
use crate::verify;

/// Outcome of a full pipeline run.
#[derive(Debug)]
pub enum PipelineOutcome {
    Success,
    PhaseFailed {
        phase: PhaseName,
        exit_code: i32,
    },
    VerifyExhausted,
    ReviewRejected {
        round: u32,
    },
}

/// Named phases for identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseName {
    Spec,
    Implement,
    Test,
    Verify,
    Review,
}

impl std::fmt::Display for PhaseName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spec => write!(f, "spec"),
            Self::Implement => write!(f, "implement"),
            Self::Test => write!(f, "test"),
            Self::Verify => write!(f, "verify"),
            Self::Review => write!(f, "review"),
        }
    }
}

/// The pipeline orchestrator. Runs multiple isolated Claude instances
/// in sequence to ensure quality through separation of concerns.
pub struct Pipeline<R: CommandRunner> {
    pub config: Config,
    pub prompt: String,
    pub spec_path: String,
    pub verify_cmd: Option<String>,
    pub base_session: String,
    pub extra_args: Vec<String>,
    pub cmd: R,
}

impl<R: CommandRunner + Clone> Pipeline<R> {
    /// Create a ClaudeRunner for a specific phase.
    fn runner_for(&self, phase: PhaseName) -> ClaudeRunner<R> {
        ClaudeRunner {
            config: self.config.clone(),
            session_name: format!("{}-{}", self.base_session, phase),
            extra_args: self.extra_args.clone(),
            cmd: self.cmd.clone(),
        }
    }

    /// Run the full pipeline: spec → implement → test → verify → review.
    pub async fn run(&self) -> PipelineOutcome {
        // ── Phase 1: Spec ──────────────────────────────────────────
        output::pipeline_phase(PhaseName::Spec, "Generating specification");

        let spec_prompt = format!(
            "Based on the following request, write a detailed specification document \
             at `{spec_path}`. The spec MUST include:\n\
             \n\
             1. **Overview** — what is being built and why\n\
             2. **Requirements** — numbered list of concrete requirements\n\
             3. **Acceptance Criteria** — for each requirement, testable pass/fail criteria\n\
             4. **File Changes** — which files to create/modify\n\
             5. **Edge Cases** — potential failure modes to handle\n\
             6. **Verification** — how to deterministically verify the implementation is correct\n\
             \n\
             Be precise and unambiguous. Each requirement should be independently verifiable.\n\
             \n\
             Request: {prompt}",
            spec_path = self.spec_path,
            prompt = self.prompt,
        );

        let spec_runner = self.runner_for(PhaseName::Spec);
        if let Err(e) = spec_runner.run_with_retry(&spec_prompt, false).await {
            return PipelineOutcome::PhaseFailed {
                phase: PhaseName::Spec,
                exit_code: e.exit_code(),
            };
        }
        output::pipeline_phase_done(PhaseName::Spec);

        // ── Phase 2: Implement ─────────────────────────────────────
        output::pipeline_phase(PhaseName::Implement, "Implementing from spec");

        let impl_prompt = format!(
            "Read the specification at `{spec_path}` and implement it exactly. \
             Follow every requirement precisely. \
             Do NOT write tests — a separate instance will handle testing. \
             Do NOT modify the spec file. \
             Focus solely on the implementation.",
            spec_path = self.spec_path,
        );

        let impl_runner = self.runner_for(PhaseName::Implement);
        if let Err(e) = impl_runner.run_with_retry(&impl_prompt, false).await {
            return PipelineOutcome::PhaseFailed {
                phase: PhaseName::Implement,
                exit_code: e.exit_code(),
            };
        }
        output::pipeline_phase_done(PhaseName::Implement);

        // ── Phase 3: Test ──────────────────────────────────────────
        output::pipeline_phase(PhaseName::Test, "Writing tests from spec");

        let test_prompt = format!(
            "Read the specification at `{spec_path}`. \
             Write comprehensive tests that verify each requirement and \
             acceptance criterion in the spec. \
             \n\
             IMPORTANT: Write tests based SOLELY on the spec — test the \
             public interface and expected behavior, not implementation details. \
             Cover all edge cases listed in the spec. \
             \n\
             Each test should map to a specific requirement number in the spec. \
             Add a comment like `// Req #N` above each test.",
            spec_path = self.spec_path,
        );

        let test_runner = self.runner_for(PhaseName::Test);
        if let Err(e) = test_runner.run_with_retry(&test_prompt, false).await {
            return PipelineOutcome::PhaseFailed {
                phase: PhaseName::Test,
                exit_code: e.exit_code(),
            };
        }
        output::pipeline_phase_done(PhaseName::Test);

        // ── Phase 4: Verify (deterministic) ────────────────────────
        if let Some(verify_cmd) = &self.verify_cmd {
            output::pipeline_phase(PhaseName::Verify, "Running verification loop");

            // Use the impl runner for fixes (it has the implementation context)
            match verify::run_verify_loop(&impl_runner, verify_cmd).await {
                verify::VerifyOutcome::Passed { .. } => {
                    output::pipeline_phase_done(PhaseName::Verify);
                }
                verify::VerifyOutcome::ExhaustedRounds => {
                    return PipelineOutcome::VerifyExhausted;
                }
                verify::VerifyOutcome::ClaudeFailed { exit_code, .. } => {
                    return PipelineOutcome::PhaseFailed {
                        phase: PhaseName::Verify,
                        exit_code,
                    };
                }
            }
        }

        // ── Phase 5: Review (independent instance) ─────────────────
        output::pipeline_phase(PhaseName::Review, "Independent review against spec");

        let review_outcome = self.run_review(&impl_runner).await;
        match review_outcome {
            ReviewOutcome::Approved => {
                output::pipeline_phase_done(PhaseName::Review);
            }
            ReviewOutcome::Rejected { round } => {
                return PipelineOutcome::ReviewRejected { round };
            }
            ReviewOutcome::Failed { exit_code } => {
                return PipelineOutcome::PhaseFailed {
                    phase: PhaseName::Review,
                    exit_code,
                };
            }
        }

        PipelineOutcome::Success
    }

    /// Run the review phase: an independent instance checks implementation
    /// against spec. If it finds issues, the impl instance fixes them,
    /// then review runs again.
    async fn run_review(&self, impl_runner: &ClaudeRunner<R>) -> ReviewOutcome {
        let max_rounds = self.config.pipeline_review_rounds;

        for round in 1..=max_rounds {
            output::review_round(round, max_rounds);

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

            let review_runner = ClaudeRunner {
                config: self.config.clone(),
                // Each review round gets its own session to avoid context contamination
                session_name: format!("{}-review-r{}", self.base_session, round),
                extra_args: self.extra_args.clone(),
                cmd: self.cmd.clone(),
            };

            // Run review with output capture
            let review_args = review_runner.build_args_with_output_format(&review_prompt, false);
            let result = match review_runner.cmd.run_claude(&review_args).await {
                Ok(r) => r,
                Err(_) => {
                    return ReviewOutcome::Failed { exit_code: 1 };
                }
            };

            if result.exit_code != 0 {
                return ReviewOutcome::Failed {
                    exit_code: result.exit_code,
                };
            }

            // Check verdict in stdout (when using --output-format json we get stdout)
            // Fall back to checking if review passed based on output
            let combined = format!("{}{}", result.stdout, result.stderr);
            if combined.contains("PIPELINE_VERDICT: PASS") {
                output::review_passed();
                return ReviewOutcome::Approved;
            }

            // Review found issues — send them to the impl instance to fix
            output::review_found_issues(round);

            let fix_prompt = format!(
                "An independent reviewer found issues with your implementation. \
                 Fix ALL of the following issues:\n\n{combined}\n\n\
                 Refer back to the spec at `{spec_path}` to ensure compliance.",
                spec_path = self.spec_path,
            );

            if let Err(e) = impl_runner.run_with_retry(&fix_prompt, true).await {
                return ReviewOutcome::Failed {
                    exit_code: e.exit_code(),
                };
            }

            // If there's a verify command, re-verify after fixes
            if let Some(verify_cmd) = &self.verify_cmd {
                match verify::run_verify_loop(impl_runner, verify_cmd).await {
                    verify::VerifyOutcome::Passed { .. } => {}
                    verify::VerifyOutcome::ExhaustedRounds => {
                        return ReviewOutcome::Failed { exit_code: 1 };
                    }
                    verify::VerifyOutcome::ClaudeFailed { exit_code, .. } => {
                        return ReviewOutcome::Failed { exit_code };
                    }
                }
            }
        }

        // Exhausted review rounds
        output::review_exhausted(max_rounds);
        ReviewOutcome::Rejected {
            round: max_rounds,
        }
    }
}

#[derive(Debug)]
enum ReviewOutcome {
    Approved,
    Rejected { round: u32 },
    Failed { exit_code: i32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RunResult;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[derive(Clone)]
    struct MockPipelineRunner {
        inner: Arc<MockPipelineRunnerInner>,
    }

    struct MockPipelineRunnerInner {
        claude_results: std::sync::Mutex<Vec<RunResult>>,
        shell_results: std::sync::Mutex<Vec<RunResult>>,
        claude_calls: AtomicU32,
        shell_calls: AtomicU32,
        claude_prompts: std::sync::Mutex<Vec<String>>,
    }

    impl MockPipelineRunner {
        fn new(claude: Vec<RunResult>, shell: Vec<RunResult>) -> Self {
            Self {
                inner: Arc::new(MockPipelineRunnerInner {
                    claude_results: std::sync::Mutex::new(claude),
                    shell_results: std::sync::Mutex::new(shell),
                    claude_calls: AtomicU32::new(0),
                    shell_calls: AtomicU32::new(0),
                    claude_prompts: std::sync::Mutex::new(Vec::new()),
                }),
            }
        }

        fn claude_calls(&self) -> u32 {
            self.inner.claude_calls.load(Ordering::SeqCst)
        }

        fn prompts(&self) -> Vec<String> {
            self.inner.claude_prompts.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl CommandRunner for MockPipelineRunner {
        async fn run_claude(&self, args: &[String]) -> std::io::Result<RunResult> {
            self.inner.claude_calls.fetch_add(1, Ordering::SeqCst);
            // Capture the prompt (first non-flag arg after -p)
            if let Some(prompt_idx) = args.iter().position(|a| a == "-p") {
                // The prompt is typically after --permission-mode bypassPermissions
                if let Some(prompt) = args.get(prompt_idx + 3) {
                    self.inner
                        .claude_prompts
                        .lock()
                        .unwrap()
                        .push(prompt.clone());
                }
            }
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

    #[tokio::test]
    async fn pipeline_success_no_verify() {
        // Spec, Impl, Test all succeed. Review passes on first try.
        let mock = MockPipelineRunner::new(
            vec![
                // Phase 1: Spec
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Phase 2: Implement
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Phase 3: Test
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Phase 5: Review — passes
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: PASS".into(),
                    stderr: String::new(),
                },
            ],
            vec![],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login feature".into(),
            spec_path: ".claude-run/spec.md".into(),
            verify_cmd: None,
            base_session: "test-pipeline".into(),
            extra_args: vec![],
            cmd: mock.clone(),
        };

        let outcome = pipeline.run().await;
        assert!(matches!(outcome, PipelineOutcome::Success));
        // Spec + Impl + Test + Review = 4 claude calls
        assert_eq!(mock.claude_calls(), 4);
    }

    #[tokio::test]
    async fn pipeline_success_with_verify() {
        let mock = MockPipelineRunner::new(
            vec![
                // Spec
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Implement
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Test write
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Review passes
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: PASS".into(),
                    stderr: String::new(),
                },
            ],
            vec![
                // Verify passes first try
                RunResult {
                    exit_code: 0,
                    stdout: "all tests pass".into(),
                    stderr: String::new(),
                },
            ],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login feature".into(),
            spec_path: ".claude-run/spec.md".into(),
            verify_cmd: Some("make test".into()),
            base_session: "test-pipeline".into(),
            extra_args: vec![],
            cmd: mock,
        };

        let outcome = pipeline.run().await;
        assert!(matches!(outcome, PipelineOutcome::Success));
    }

    #[tokio::test]
    async fn pipeline_spec_fails() {
        let mock = MockPipelineRunner::new(
            vec![RunResult {
                exit_code: 2,
                stdout: String::new(),
                stderr: "error".into(),
            }],
            vec![],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: ".claude-run/spec.md".into(),
            verify_cmd: None,
            base_session: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };

        let outcome = pipeline.run().await;
        match outcome {
            PipelineOutcome::PhaseFailed { phase, exit_code } => {
                assert_eq!(phase, PhaseName::Spec);
                assert_eq!(exit_code, 2);
            }
            other => panic!("expected PhaseFailed(Spec), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pipeline_review_rejects_then_approves() {
        let mock = MockPipelineRunner::new(
            vec![
                // Spec
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Implement
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Test
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Review round 1: FAIL
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: FAIL\n1. Missing input validation".into(),
                    stderr: String::new(),
                },
                // Impl fix (resume)
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

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: ".claude-run/spec.md".into(),
            verify_cmd: None,
            base_session: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };

        let outcome = pipeline.run().await;
        assert!(matches!(outcome, PipelineOutcome::Success));
    }

    #[tokio::test]
    async fn pipeline_sessions_are_isolated() {
        let mock = MockPipelineRunner::new(
            vec![
                // Spec
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Implement
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Test
                RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                // Review
                RunResult {
                    exit_code: 0,
                    stdout: "PIPELINE_VERDICT: PASS".into(),
                    stderr: String::new(),
                },
            ],
            vec![],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: ".claude-run/spec.md".into(),
            verify_cmd: None,
            base_session: "my-task".into(),
            extra_args: vec![],
            cmd: mock,
        };

        let outcome = pipeline.run().await;
        assert!(matches!(outcome, PipelineOutcome::Success));

        // Verify session names are isolated per phase
        let spec_runner = pipeline.runner_for(PhaseName::Spec);
        assert_eq!(spec_runner.session_name, "my-task-spec");

        let impl_runner = pipeline.runner_for(PhaseName::Implement);
        assert_eq!(impl_runner.session_name, "my-task-implement");

        let test_runner = pipeline.runner_for(PhaseName::Test);
        assert_eq!(test_runner.session_name, "my-task-test");
    }
}
