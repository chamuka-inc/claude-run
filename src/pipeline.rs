use crate::config::Config;
use crate::output;
use crate::runner::{ClaudeRunner, CommandRunner};
use crate::verify::{self, ShellVerifier, SpecReviewVerifier, Verifier, VerifyOutcome};

/// Outcome of a full pipeline run.
#[derive(Debug)]
pub enum PipelineOutcome {
    Success,
    PhaseFailed {
        phase: PhaseName,
        exit_code: i32,
    },
    VerifierExhausted {
        verifier_name: String,
    },
}

/// Named phases for identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseName {
    Spec,
    Implement,
    Test,
}

impl std::fmt::Display for PhaseName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spec => write!(f, "spec"),
            Self::Implement => write!(f, "implement"),
            Self::Test => write!(f, "test"),
        }
    }
}

/// The pipeline orchestrator. Runs multiple isolated Claude instances
/// in sequence, then gates quality through verifiers.
///
/// Architecture:
///   Phases produce work:  spec → implement → test
///   Verifiers gate quality:  [shell verify] → [spec review]
///
/// Every verifier follows the same loop: check → fail? → fix → recheck.
/// The only difference is what "check" means (shell command vs Claude review).
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

    /// Run the full pipeline: phases produce work, verifiers gate quality.
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

        // ── Verifiers gate quality ─────────────────────────────────
        // Each verifier follows the same loop: check → fail → fix → recheck.
        // The impl_runner is the fixer for all verifiers.

        // Verifier 1: Shell command (deterministic — tests pass/fail)
        if let Some(verify_cmd) = &self.verify_cmd {
            let shell_verifier = ShellVerifier::new(verify_cmd, self.cmd.clone());
            let outcome = verify::run_verify_loop(
                &shell_verifier,
                &impl_runner,
                self.config.verify_max,
            )
            .await;
            if let Some(failure) = check_outcome(outcome, shell_verifier.name()) {
                return failure;
            }
        }

        // Verifier 2: Spec review (independent Claude instance)
        let review_verifier = SpecReviewVerifier::new(
            &self.spec_path,
            &self.base_session,
            self.config.clone(),
            self.extra_args.clone(),
            self.cmd.clone(),
        );
        let outcome = verify::run_verify_loop(
            &review_verifier,
            &impl_runner,
            self.config.pipeline_review_rounds,
        )
        .await;
        if let Some(failure) = check_outcome(outcome, review_verifier.name()) {
            return failure;
        }

        PipelineOutcome::Success
    }
}

/// Convert a VerifyOutcome into a PipelineOutcome failure, or None if passed.
fn check_outcome(outcome: VerifyOutcome, name: &str) -> Option<PipelineOutcome> {
    match outcome {
        VerifyOutcome::Passed { .. } => None,
        VerifyOutcome::ExhaustedRounds => Some(PipelineOutcome::VerifierExhausted {
            verifier_name: name.to_string(),
        }),
        VerifyOutcome::FixerFailed { exit_code, .. } => Some(PipelineOutcome::PhaseFailed {
            phase: PhaseName::Implement, // fixer is always the impl runner
            exit_code,
        }),
    }
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
    }

    impl MockPipelineRunner {
        fn new(claude: Vec<RunResult>, shell: Vec<RunResult>) -> Self {
            Self {
                inner: Arc::new(MockPipelineRunnerInner {
                    claude_results: std::sync::Mutex::new(claude),
                    shell_results: std::sync::Mutex::new(shell),
                    claude_calls: AtomicU32::new(0),
                }),
            }
        }

        fn claude_calls(&self) -> u32 {
            self.inner.claude_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl CommandRunner for MockPipelineRunner {
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
        // Spec + Impl + Test succeed, Review passes
        let mock = MockPipelineRunner::new(
            vec![
                ok_result(),          // Spec
                ok_result(),          // Implement
                ok_result(),          // Test
                review_pass_result(), // Review: PASS
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
        assert_eq!(mock.claude_calls(), 4); // Spec + Impl + Test + Review
    }

    #[tokio::test]
    async fn pipeline_success_with_verify() {
        let mock = MockPipelineRunner::new(
            vec![
                ok_result(),          // Spec
                ok_result(),          // Implement
                ok_result(),          // Test
                review_pass_result(), // Review: PASS
            ],
            vec![
                // Shell verify passes
                RunResult {
                    exit_code: 0,
                    stdout: "all tests pass".into(),
                    stderr: String::new(),
                },
            ],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: "spec.md".into(),
            verify_cmd: Some("make test".into()),
            base_session: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };

        assert!(matches!(pipeline.run().await, PipelineOutcome::Success));
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
            spec_path: "spec.md".into(),
            verify_cmd: None,
            base_session: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };

        match pipeline.run().await {
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
                ok_result(),          // Spec
                ok_result(),          // Implement
                ok_result(),          // Test
                review_fail_result(), // Review round 1: FAIL
                ok_result(),          // Fixer
                review_pass_result(), // Review round 2: PASS
            ],
            vec![],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: "spec.md".into(),
            verify_cmd: None,
            base_session: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };

        assert!(matches!(pipeline.run().await, PipelineOutcome::Success));
    }

    #[tokio::test]
    async fn pipeline_shell_verify_fails_then_passes() {
        let mock = MockPipelineRunner::new(
            vec![
                ok_result(),          // Spec
                ok_result(),          // Implement
                ok_result(),          // Test
                ok_result(),          // Fixer (after shell fail)
                review_pass_result(), // Review: PASS
            ],
            vec![
                // Shell round 1: fail
                RunResult {
                    exit_code: 1,
                    stdout: "test failed".into(),
                    stderr: String::new(),
                },
                // Shell round 2: pass
                RunResult {
                    exit_code: 0,
                    stdout: "ok".into(),
                    stderr: String::new(),
                },
            ],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: "spec.md".into(),
            verify_cmd: Some("make test".into()),
            base_session: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };

        assert!(matches!(pipeline.run().await, PipelineOutcome::Success));
    }

    #[tokio::test]
    async fn pipeline_sessions_are_isolated() {
        let mock = MockPipelineRunner::new(
            vec![ok_result(), ok_result(), ok_result(), review_pass_result()],
            vec![],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: "spec.md".into(),
            verify_cmd: None,
            base_session: "my-task".into(),
            extra_args: vec![],
            cmd: mock,
        };

        pipeline.run().await;

        assert_eq!(pipeline.runner_for(PhaseName::Spec).session_name, "my-task-spec");
        assert_eq!(pipeline.runner_for(PhaseName::Implement).session_name, "my-task-implement");
        assert_eq!(pipeline.runner_for(PhaseName::Test).session_name, "my-task-test");
    }

    #[tokio::test]
    async fn pipeline_verifier_exhausted_reports_name() {
        let mock = MockPipelineRunner::new(
            vec![
                ok_result(), // Spec
                ok_result(), // Implement
                ok_result(), // Test
                // Review keeps failing
                review_fail_result(),
                ok_result(), // Fixer
                review_fail_result(),
                ok_result(), // Fixer
                review_fail_result(), // Final check
            ],
            vec![],
        );

        let pipeline = Pipeline {
            config: test_config(),
            prompt: "add login".into(),
            spec_path: "spec.md".into(),
            verify_cmd: None,
            base_session: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };

        match pipeline.run().await {
            PipelineOutcome::VerifierExhausted { verifier_name } => {
                assert_eq!(verifier_name, "spec review");
            }
            other => panic!("expected VerifierExhausted, got {other:?}"),
        }
    }

    // ── Test helpers ───────────────────────────────────────────

    fn ok_result() -> RunResult {
        RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn review_pass_result() -> RunResult {
        RunResult {
            exit_code: 0,
            stdout: "PIPELINE_VERDICT: PASS".into(),
            stderr: String::new(),
        }
    }

    fn review_fail_result() -> RunResult {
        RunResult {
            exit_code: 0,
            stdout: "PIPELINE_VERDICT: FAIL\n1. Missing validation".into(),
            stderr: String::new(),
        }
    }
}
