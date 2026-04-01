use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::Config;
use crate::notify;
use crate::output;
use crate::rate_limit::{is_rate_limited, Backoff};

/// Result of running a subprocess.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Trait abstracting subprocess execution for testability.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Run claude with the given arguments. Streams stdout/stderr to terminal.
    /// Returns exit code and captured stderr.
    async fn run_claude(&self, args: &[String]) -> std::io::Result<RunResult>;

    /// Run a shell command (for verification). Returns exit code and combined output.
    async fn run_shell(&self, cmd: &str) -> std::io::Result<RunResult>;
}

/// Real implementation using tokio::process.
#[derive(Clone)]
pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run_claude(&self, args: &[String]) -> std::io::Result<RunResult> {
        let mut child = Command::new("claude")
            .args(args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()?;

        // Capture stderr while streaming it to the terminal
        let stderr_handle = child.stderr.take().unwrap();
        let mut stderr_reader = BufReader::new(stderr_handle).lines();
        let mut stderr_buf = String::new();

        while let Some(line) = stderr_reader.next_line().await? {
            eprintln!("{line}");
            stderr_buf.push_str(&line);
            stderr_buf.push('\n');
        }

        let status = child.wait().await?;
        Ok(RunResult {
            exit_code: status.code().unwrap_or(1),
            stdout: String::new(), // stdout goes directly to terminal
            stderr: stderr_buf,
        })
    }

    async fn run_shell(&self, cmd: &str) -> std::io::Result<RunResult> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .await?;

        // Also capture for the fix prompt (re-run capturing output)
        let captured = Command::new("sh").arg("-c").arg(cmd).output().await?;

        Ok(RunResult {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&captured.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&captured.stderr).into_owned(),
        })
    }
}

/// The main orchestrator that runs Claude with retry logic.
pub struct ClaudeRunner<R: CommandRunner> {
    pub config: Config,
    pub session_name: String,
    pub extra_args: Vec<String>,
    pub cmd: R,
}

impl<R: CommandRunner> ClaudeRunner<R> {
    /// Build the argument list for a claude invocation.
    pub fn build_args(&self, prompt: &str, is_resume: bool) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
        ];

        if is_resume {
            args.push("--continue".to_string());
        }

        args.push(prompt.to_string());

        if !self.session_name.is_empty() {
            args.push("--name".to_string());
            args.push(self.session_name.clone());
        }

        args.extend(self.extra_args.iter().cloned());
        args
    }

    /// Build args with --output-format stream-json for capturing structured output.
    /// Used by pipeline review phase to capture the verdict.
    pub fn build_args_with_output_format(&self, prompt: &str, is_resume: bool) -> Vec<String> {
        let mut args = self.build_args(prompt, is_resume);
        args.push("--output-format".to_string());
        args.push("stream-json".to_string());
        args
    }

    /// Run claude with automatic rate-limit retry and backoff.
    pub async fn run_with_retry(&self, prompt: &str, is_resume: bool) -> Result<(), RunError> {
        let mut backoff = Backoff::new(self.config.retry_delay, self.config.retry_cap);
        let mut attempt: u32 = 0;
        let mut resume = is_resume;
        let mut current_prompt = prompt.to_string();

        loop {
            let args = self.build_args(&current_prompt, resume);
            let result = self.cmd.run_claude(&args).await.map_err(RunError::Io)?;

            if result.exit_code == 0 {
                return Ok(());
            }

            if is_rate_limited(result.exit_code, &result.stderr) {
                attempt += 1;

                if attempt > self.config.max_retries {
                    // Fast retries exhausted — try daily cap polling
                    self.wait_for_cap_reset().await?;
                    attempt = 0;
                    backoff.reset();
                    resume = true;
                    current_prompt = "continue where you left off".to_string();
                    output::resuming(&self.session_name);
                    continue;
                }

                let delay = backoff.next_delay();
                output::rate_limited(attempt, self.config.max_retries, delay.as_secs());
                tokio::time::sleep(delay).await;
                resume = true;
                current_prompt = "continue where you left off".to_string();
                output::resuming(&self.session_name);
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
                self.session_name
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
                    &format!("Rate limit lifted — resuming: {}", self.session_name),
                    self.config.notify,
                );
                return Ok(());
            }

            if !is_rate_limited(result.exit_code, &result.stderr) {
                return Err(RunError::ClaudeFailed(result.exit_code));
            }
        }

        notify::notify(
            &format!("Timed out waiting for cap reset: {}", self.session_name),
            self.config.notify,
        );
        Err(RunError::DailyCapTimeout)
    }
}

#[derive(Debug)]
pub enum RunError {
    Io(std::io::Error),
    ClaudeFailed(i32),
    DailyCapTimeout,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::ClaudeFailed(code) => write!(f, "Claude exited with code {code}"),
            Self::DailyCapTimeout => write!(f, "Timed out waiting for daily cap reset"),
        }
    }
}

impl std::error::Error for RunError {}

impl RunError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) => 1,
            Self::ClaudeFailed(code) => *code,
            Self::DailyCapTimeout => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Mock runner that returns pre-configured results.
    struct MockRunner {
        results: std::sync::Mutex<Vec<RunResult>>,
        call_count: AtomicU32,
    }

    impl MockRunner {
        fn new(results: Vec<RunResult>) -> Self {
            Self {
                results: std::sync::Mutex::new(results),
                call_count: AtomicU32::new(0),
            }
        }

        fn calls(&self) -> u32 {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CommandRunner for Arc<MockRunner> {
        async fn run_claude(&self, _args: &[String]) -> std::io::Result<RunResult> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
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
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
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
            retry_delay: std::time::Duration::from_millis(1), // fast for tests
            retry_cap: std::time::Duration::from_millis(10),
            notify: false,
            verify_max: 3,
            daily_cap_poll: std::time::Duration::from_millis(1),
            daily_cap_timeout: std::time::Duration::from_millis(10),
            pipeline_review_rounds: 2,
        }
    }

    #[tokio::test]
    async fn success_on_first_try() {
        let mock = Arc::new(MockRunner::new(vec![RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }]));
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock.clone(),
        };
        let result = runner.run_with_retry("do something", false).await;
        assert!(result.is_ok());
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn retries_on_rate_limit_then_succeeds() {
        let mock = Arc::new(MockRunner::new(vec![
            RunResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: "rate limit exceeded".into(),
            },
            RunResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: "429 too many requests".into(),
            },
            RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
        ]));
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock.clone(),
        };
        let result = runner.run_with_retry("do something", false).await;
        assert!(result.is_ok());
        assert_eq!(mock.calls(), 3);
    }

    #[tokio::test]
    async fn non_rate_limit_error_fails_immediately() {
        let mock = Arc::new(MockRunner::new(vec![RunResult {
            exit_code: 2,
            stdout: String::new(),
            stderr: "unknown error".into(),
        }]));
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock.clone(),
        };
        let result = runner.run_with_retry("do something", false).await;
        assert!(result.is_err());
        assert_eq!(mock.calls(), 1);
        match result.unwrap_err() {
            RunError::ClaudeFailed(code) => assert_eq!(code, 2),
            other => panic!("expected ClaudeFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_args_normal() {
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "my-session".into(),
            extra_args: vec!["--max-turns".into(), "50".into()],
            cmd: Arc::new(MockRunner::new(vec![])),
        };
        let args = runner.build_args("do something", false);
        assert_eq!(
            args,
            vec![
                "-p",
                "--permission-mode",
                "bypassPermissions",
                "do something",
                "--name",
                "my-session",
                "--max-turns",
                "50",
            ]
        );
    }

    #[tokio::test]
    async fn build_args_resume() {
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "my-session".into(),
            extra_args: vec![],
            cmd: Arc::new(MockRunner::new(vec![])),
        };
        let args = runner.build_args("continue", true);
        assert!(args.contains(&"--continue".to_string()));
    }

    #[tokio::test]
    async fn daily_cap_timeout() {
        // All retries rate-limited, daily cap probe also rate-limited -> timeout
        let mut results = Vec::new();
        // max_retries + 1 rate-limited responses (to exhaust fast retries)
        for _ in 0..4 {
            results.push(RunResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: "rate limit exceeded".into(),
            });
        }
        // Daily cap probes also rate-limited (enough to exceed timeout)
        for _ in 0..20 {
            results.push(RunResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: "rate limit exceeded".into(),
            });
        }

        let mock = Arc::new(MockRunner::new(results));
        let runner = ClaudeRunner {
            config: test_config(),
            session_name: "test".into(),
            extra_args: vec![],
            cmd: mock,
        };
        let result = runner.run_with_retry("do something", false).await;
        assert!(matches!(result, Err(RunError::DailyCapTimeout)));
    }
}
