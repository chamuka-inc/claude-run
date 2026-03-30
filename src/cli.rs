use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "claude-run",
    version,
    about = "Run Claude Code non-interactively with automatic rate-limit retry",
    after_help = "Environment variables:\n  \
        CLAUDE_MAX_RETRIES     Max rate-limit retries         (default: 10)\n  \
        CLAUDE_RETRY_DELAY     Initial backoff in seconds     (default: 60)\n  \
        CLAUDE_RETRY_CAP       Max backoff in seconds         (default: 300)\n  \
        CLAUDE_NOTIFY          macOS notification on done     (default: 1)\n  \
        CLAUDE_VERIFY_MAX      Max verify-fix cycles          (default: 5)\n  \
        CLAUDE_DAILY_CAP_POLL  Poll interval for daily cap    (default: 300)\n  \
        CLAUDE_DAILY_CAP_TIMEOUT  Max wait for cap reset      (default: 28800)"
)]
pub struct Cli {
    /// The prompt to send to Claude
    pub prompt: Option<String>,

    /// Session name (default: auto-generated from prompt)
    #[arg(long)]
    pub name: Option<String>,

    /// Resume last session, or a named session
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// After Claude finishes, run CMD to verify. If it fails, send Claude back to fix.
    #[arg(long)]
    pub verify: Option<String>,

    /// Extra arguments passed through to claude (e.g. --max-turns 50)
    #[arg(last = true)]
    pub extra: Vec<String>,
}

impl Cli {
    pub fn validate(&self) -> Result<(), String> {
        if self.resume.is_none() && self.prompt.is_none() {
            return Err("No prompt provided. Usage: claude-run \"your prompt\" or claude-run --resume [session-name]".into());
        }
        Ok(())
    }

    pub fn is_resume(&self) -> bool {
        self.resume.is_some()
    }

    pub fn resume_target(&self) -> Option<&str> {
        self.resume.as_deref().filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("claude-run").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn parse_simple_prompt() {
        let cli = parse(&["implement login"]);
        assert_eq!(cli.prompt.as_deref(), Some("implement login"));
        assert!(!cli.is_resume());
    }

    #[test]
    fn parse_name_and_prompt() {
        let cli = parse(&["--name", "foo", "implement login"]);
        assert_eq!(cli.name.as_deref(), Some("foo"));
        assert_eq!(cli.prompt.as_deref(), Some("implement login"));
    }

    #[test]
    fn parse_resume_no_target() {
        let cli = parse(&["--resume"]);
        assert!(cli.is_resume());
        assert_eq!(cli.resume_target(), None);
    }

    #[test]
    fn parse_resume_with_target() {
        let cli = parse(&["--resume", "my-session"]);
        assert!(cli.is_resume());
        assert_eq!(cli.resume_target(), Some("my-session"));
    }

    #[test]
    fn parse_verify_and_prompt() {
        let cli = parse(&["--verify", "make test", "implement login"]);
        assert_eq!(cli.verify.as_deref(), Some("make test"));
        assert_eq!(cli.prompt.as_deref(), Some("implement login"));
    }

    #[test]
    fn parse_extra_args() {
        let cli = parse(&["prompt here", "--", "--max-turns", "50"]);
        assert_eq!(cli.extra, vec!["--max-turns", "50"]);
    }

    #[test]
    fn validate_no_prompt_no_resume() {
        let cli = parse(&["--name", "foo"]);
        assert!(cli.validate().is_err());
    }

    #[test]
    fn validate_prompt_ok() {
        let cli = parse(&["do something"]);
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn validate_resume_ok() {
        let cli = parse(&["--resume"]);
        assert!(cli.validate().is_ok());
    }
}
