use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

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

    /// Run claude with stdout captured (for verdict parsing).
    /// Stderr is still streamed to terminal. Stdout is tee'd (printed + captured).
    async fn run_claude_capturing(&self, args: &[String]) -> std::io::Result<RunResult>;

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

    async fn run_claude_capturing(&self, args: &[String]) -> std::io::Result<RunResult> {
        let mut child = Command::new("claude")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Capture stdout while tee-ing to terminal
        let stdout_handle = child.stdout.take().unwrap();
        let mut stdout_reader = BufReader::new(stdout_handle).lines();
        let mut stdout_buf = String::new();

        let stderr_handle = child.stderr.take().unwrap();
        let mut stderr_reader = BufReader::new(stderr_handle).lines();
        let mut stderr_buf = String::new();

        // Read both streams (simple sequential — fine for CLI output)
        loop {
            tokio::select! {
                line = stdout_reader.next_line() => {
                    match line? {
                        Some(line) => {
                            println!("{line}");
                            stdout_buf.push_str(&line);
                            stdout_buf.push('\n');
                        }
                        None => break,
                    }
                }
                line = stderr_reader.next_line() => {
                    if let Some(line) = line? {
                        eprintln!("{line}");
                        stderr_buf.push_str(&line);
                        stderr_buf.push('\n');
                    }
                }
            }
        }

        // Drain remaining stderr
        while let Some(line) = stderr_reader.next_line().await? {
            eprintln!("{line}");
            stderr_buf.push_str(&line);
            stderr_buf.push('\n');
        }

        let status = child.wait().await?;
        Ok(RunResult {
            exit_code: status.code().unwrap_or(1),
            stdout: stdout_buf,
            stderr: stderr_buf,
        })
    }

    async fn run_shell(&self, cmd: &str) -> std::io::Result<RunResult> {
        // Run once, capture output, and tee to terminal
        let output = Command::new("sh").arg("-c").arg(cmd).output().await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Stream captured output to terminal
        if !stdout.is_empty() {
            print!("{stdout}");
        }
        if !stderr.is_empty() {
            eprint!("{stderr}");
        }

        Ok(RunResult {
            exit_code: output.status.code().unwrap_or(1),
            stdout: stdout.into_owned(),
            stderr: stderr.into_owned(),
        })
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
