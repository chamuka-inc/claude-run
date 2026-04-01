/// Command-line arguments for claude-run.
///
/// Uses manual parsing (not clap) so that unknown flags pass through
/// directly to `claude` without requiring a `--` separator.
///
/// Known flags:
///   --name NAME      Session name
///   --resume [NAME]  Resume a session
///   --verify CMD     Verification command
///   --help, -h       Show help
///   --version, -v    Show version
///
/// Everything else (unknown flags and the positional prompt) passes through.
#[derive(Debug, Default)]
pub struct Cli {
    pub prompt: Option<String>,
    pub name: Option<String>,
    pub resume: Option<String>,
    pub verify: Option<String>,
    pub pipeline: bool,
    pub spec: Option<String>,
    pub extra: Vec<String>,
}

const HELP_TEXT: &str = "\
Usage: claude-run [OPTIONS] \"prompt\"
       claude-run --resume [session-name]
       claude-run --pipeline [--verify CMD] \"prompt\"

Run Claude Code non-interactively with automatic rate-limit retry.

Options:
  --name NAME        Session name (default: auto-generated from prompt)
  --resume [NAME]    Resume last session, or a named session
  --verify CMD       After Claude finishes, run CMD to verify. If it fails,
                     send Claude back in with the output to fix it.
  --pipeline         Autonomous multi-instance pipeline: spec → implement →
                     test → verify → review. Each phase uses an isolated
                     Claude session for quality through separation of concerns.
  --spec PATH        Spec file path (default: .claude-run/spec.md). With
                     --pipeline, skip spec generation and use existing spec.
  --help, -h         Show this help
  --version, -v      Show version

All other flags are passed through to claude (e.g. --max-turns 50, --model opus).

Environment variables:
  CLAUDE_MAX_RETRIES              Max rate-limit retries         (default: 10)
  CLAUDE_RETRY_DELAY              Initial backoff in seconds     (default: 60)
  CLAUDE_RETRY_CAP                Max backoff in seconds         (default: 300)
  CLAUDE_NOTIFY                   macOS notification on done     (default: 1)
  CLAUDE_VERIFY_MAX               Max verify-fix cycles          (default: 5)
  CLAUDE_DAILY_CAP_POLL           Poll interval for daily cap    (default: 300)
  CLAUDE_DAILY_CAP_TIMEOUT        Max wait for cap reset         (default: 28800)
  CLAUDE_PIPELINE_REVIEW_ROUNDS   Max review-fix rounds          (default: 3)

Examples:
  claude-run \"implement the login feature\"
  claude-run --name login-feat \"implement the login feature\"
  claude-run --verify \"make ci\" \"implement the login feature\"
  claude-run --pipeline --verify \"make ci\" \"implement the login feature\"
  claude-run --pipeline --spec ./spec.md \"implement from this spec\"
  claude-run --resume
  claude-run --resume login-feat";

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parse result — either a valid Cli or an early exit (help/version).
pub enum ParseResult {
    Ok(Cli),
    Exit { message: String, code: i32 },
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> ParseResult {
    let mut cli = Cli::default();
    let mut args = args.into_iter().peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                return ParseResult::Exit {
                    message: HELP_TEXT.to_string(),
                    code: 0,
                };
            }
            "--version" | "-v" => {
                return ParseResult::Exit {
                    message: format!("claude-run {VERSION}"),
                    code: 0,
                };
            }
            "--name" => {
                cli.name = args.next();
            }
            "--verify" => {
                cli.verify = args.next();
            }
            "--pipeline" => {
                cli.pipeline = true;
            }
            "--spec" => {
                cli.spec = args.next();
            }
            "--resume" => {
                // --resume takes an optional non-flag argument
                let target = args.peek().filter(|next| !next.starts_with('-')).cloned();
                if target.is_some() {
                    args.next(); // consume the peeked value
                }
                cli.resume = Some(target.unwrap_or_default());
            }
            _ if arg.starts_with('-') => {
                // Unknown flag — pass through to claude
                cli.extra.push(arg);
                // If the next arg looks like a value (not a flag), take it too
                if let Some(next) = args.peek() {
                    if !next.starts_with('-') {
                        cli.extra.push(args.next().unwrap());
                    }
                }
            }
            _ => {
                // Positional — first one is the prompt, rest go to extra
                if cli.prompt.is_none() {
                    cli.prompt = Some(arg);
                } else {
                    cli.extra.push(arg);
                }
            }
        }
    }

    ParseResult::Ok(cli)
}

/// Parse from std::env::args (skipping argv[0]).
pub fn parse_from_env() -> ParseResult {
    parse_args(std::env::args().skip(1))
}

impl Cli {
    pub fn validate(&self) -> Result<(), String> {
        if self.resume.is_none() && self.prompt.is_none() {
            return Err("Error: No prompt provided.\n\
                 Usage: claude-run \"your prompt\"\n\
                        claude-run --resume [session-name]"
                .into());
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
        match parse_args(args.iter().map(|s| s.to_string())) {
            ParseResult::Ok(cli) => cli,
            ParseResult::Exit { message, .. } => panic!("unexpected exit: {message}"),
        }
    }

    #[test]
    fn parse_simple_prompt() {
        let cli = parse(&["implement login"]);
        assert_eq!(cli.prompt.as_deref(), Some("implement login"));
        assert!(!cli.is_resume());
        assert!(cli.extra.is_empty());
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
    fn parse_unknown_flags_pass_through() {
        let cli = parse(&["--max-turns", "50", "--model", "opus", "do something"]);
        assert_eq!(cli.prompt.as_deref(), Some("do something"));
        assert_eq!(cli.extra, vec!["--max-turns", "50", "--model", "opus"]);
    }

    #[test]
    fn parse_mixed_known_and_unknown_flags() {
        let cli = parse(&[
            "--verify",
            "make ci",
            "--max-turns",
            "50",
            "--name",
            "my-task",
            "do something",
        ]);
        assert_eq!(cli.verify.as_deref(), Some("make ci"));
        assert_eq!(cli.name.as_deref(), Some("my-task"));
        assert_eq!(cli.prompt.as_deref(), Some("do something"));
        assert_eq!(cli.extra, vec!["--max-turns", "50"]);
    }

    #[test]
    fn parse_flag_without_value_at_end() {
        let cli = parse(&["do something", "--verbose"]);
        assert_eq!(cli.prompt.as_deref(), Some("do something"));
        assert_eq!(cli.extra, vec!["--verbose"]);
    }

    #[test]
    fn parse_resume_followed_by_flag() {
        // --resume followed by a flag should not consume the flag as the target
        let cli = parse(&["--resume", "--verbose"]);
        assert!(cli.is_resume());
        assert_eq!(cli.resume_target(), None);
        assert_eq!(cli.extra, vec!["--verbose"]);
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

    #[test]
    fn help_flag_exits() {
        match parse_args(["--help"].iter().map(|s| s.to_string())) {
            ParseResult::Exit { code, message } => {
                assert_eq!(code, 0);
                assert!(message.contains("claude-run"));
            }
            ParseResult::Ok(_) => panic!("expected exit"),
        }
    }

    #[test]
    fn version_flag_exits() {
        match parse_args(["--version"].iter().map(|s| s.to_string())) {
            ParseResult::Exit { code, message } => {
                assert_eq!(code, 0);
                assert!(message.contains("claude-run"));
            }
            ParseResult::Ok(_) => panic!("expected exit"),
        }
    }
}
