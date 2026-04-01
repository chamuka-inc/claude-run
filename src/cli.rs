/// Command-line arguments for claude-run.
///
/// Uses manual parsing (not clap) so that unknown flags pass through
/// directly to `claude` without requiring a `--` separator.
///
/// Known flags:
///   --name NAME              Session name
///   --resume [NAME]          Resume a session
///   --verify CMD             Verification command
///   --adversarial-verify/--av  Enable adversarial spec-compliance review
///   --av-spec FILE           Spec file for reviewer
///   --av-threshold N         Minimum score to pass (default: 95)
///   --av-rounds N            Max review-fix rounds (default: 3)
///   --av-model MODEL         Model for reviewer
///   --av-prompt FILE         Custom reviewer prompt template
///   --pipeline FILE          Load multi-stage pipeline from YAML
///   --help, -h               Show help
///   --version, -v            Show version
///
/// Everything else (unknown flags and the positional prompt) passes through.
#[derive(Debug, Default)]
pub struct Cli {
    pub prompt: Option<String>,
    pub name: Option<String>,
    pub resume: Option<String>,
    pub verify: Option<String>,
    pub pipeline: Option<String>,
    pub av: bool,
    pub av_spec: Option<String>,
    pub av_threshold: Option<u32>,
    pub av_rounds: Option<u32>,
    pub av_model: Option<String>,
    pub av_prompt: Option<String>,
    pub extra: Vec<String>,
}

const HELP_TEXT: &str = "\
Usage: claude-run [OPTIONS] \"prompt\"
       claude-run --resume [session-name]
       claude-run --pipeline pipeline.yaml

Run Claude Code non-interactively with automatic rate-limit retry.

Options:
  --name NAME        Session name (default: auto-generated from prompt)
  --resume [NAME]    Resume last session, or a named session
  --verify CMD       After Claude finishes, run CMD to verify. If it fails,
                     send Claude back in with the output to fix it.
  --pipeline FILE    Load a multi-stage pipeline from a YAML file.
                     Cannot be combined with --verify, --av, or a prompt.
  --help, -h         Show this help
  --version, -v      Show version

Adversarial Verification:
  --adversarial-verify, --av
                     Enable adversarial spec-compliance review
  --av-spec FILE     Path to the spec file the reviewer checks against
  --av-threshold N   Minimum score to pass (default: 95)
  --av-rounds N      Max review-fix rounds (default: 3)
  --av-model MODEL   Model for the reviewer (default: same as worker)
  --av-prompt FILE   Custom reviewer prompt template

All other flags are passed through to claude (e.g. --max-turns 50, --model opus).

Environment variables:
  CLAUDE_MAX_RETRIES       Max rate-limit retries         (default: 10)
  CLAUDE_RETRY_DELAY       Initial backoff in seconds     (default: 60)
  CLAUDE_RETRY_CAP         Max backoff in seconds         (default: 300)
  CLAUDE_NOTIFY            macOS notification on done     (default: 1)
  CLAUDE_VERIFY_MAX        Max verify-fix cycles          (default: 5)
  CLAUDE_DAILY_CAP_POLL    Poll interval for daily cap    (default: 300)
  CLAUDE_DAILY_CAP_TIMEOUT Max wait for cap reset         (default: 28800)
  CLAUDE_AV_THRESHOLD      Minimum spec-compliance score  (default: 95)
  CLAUDE_AV_ROUNDS         Max adversarial review rounds  (default: 3)
  CLAUDE_AV_MODEL          Override model for reviewer    (default: none)

Examples:
  claude-run \"implement the login feature\"
  claude-run --name login-feat \"implement the login feature\"
  claude-run --verify \"make ci\" \"implement the login feature\"
  claude-run --av --av-spec spec.md --verify \"make ci\" \"implement the spec\"
  claude-run --av --av-threshold 90 --av-rounds 5 \"implement the spec\"
  claude-run --pipeline pipeline.yaml
  claude-run --max-turns 50 --model opus \"implement the login feature\"
  claude-run --resume
  claude-run --resume login-feat";

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parse result — either a valid Cli or an early exit (help/version).
pub enum ParseResult {
    Ok(Box<Cli>),
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
            "--resume" => {
                // --resume takes an optional non-flag argument
                let target = args.peek().filter(|next| !next.starts_with('-')).cloned();
                if target.is_some() {
                    args.next(); // consume the peeked value
                }
                cli.resume = Some(target.unwrap_or_default());
            }
            "--adversarial-verify" | "--av" => {
                cli.av = true;
            }
            "--av-spec" => {
                cli.av_spec = args.next();
            }
            "--av-threshold" => {
                cli.av_threshold = args.next().and_then(|v| v.parse().ok());
            }
            "--av-rounds" => {
                cli.av_rounds = args.next().and_then(|v| v.parse().ok());
            }
            "--av-model" => {
                cli.av_model = args.next();
            }
            "--av-prompt" => {
                cli.av_prompt = args.next();
            }
            "--pipeline" => {
                cli.pipeline = args.next();
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

    ParseResult::Ok(Box::new(cli))
}

/// Parse from std::env::args (skipping argv[0]).
pub fn parse_from_env() -> ParseResult {
    parse_args(std::env::args().skip(1))
}

impl Cli {
    pub fn validate(&self) -> Result<(), String> {
        if self.pipeline.is_some() {
            if self.verify.is_some() || self.av {
                return Err(
                    "Error: --pipeline cannot be combined with --verify or --av.\n\
                     Use the YAML file to define verification stages."
                        .into(),
                );
            }
            if self.prompt.is_some() {
                return Err(
                    "Error: --pipeline cannot be combined with a prompt argument.\n\
                     Define the prompt in the YAML file."
                        .into(),
                );
            }
            if self.resume.is_some() {
                return Err("Error: --pipeline cannot be combined with --resume.".into());
            }
            return Ok(());
        }

        if self.resume.is_none() && self.prompt.is_none() {
            return Err("Error: No prompt provided.\n\
                 Usage: claude-run \"your prompt\"\n\
                        claude-run --resume [session-name]\n\
                        claude-run --pipeline pipeline.yaml"
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

    /// Banner info for adversarial verification, if enabled.
    pub fn av_banner(&self) -> Option<(&str, u32)> {
        if self.av {
            let spec = self.av_spec.as_deref().unwrap_or("(auto-detect)");
            let threshold = self.av_threshold.unwrap_or(95);
            Some((spec, threshold))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        match parse_args(args.iter().map(|s| s.to_string())) {
            ParseResult::Ok(cli) => *cli,
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

    // ─── Adversarial verification flag tests ───────────────────────

    #[test]
    fn parse_av_flag() {
        let cli = parse(&["--av", "implement the spec"]);
        assert!(cli.av);
        assert_eq!(cli.prompt.as_deref(), Some("implement the spec"));
    }

    #[test]
    fn parse_adversarial_verify_long_form() {
        let cli = parse(&["--adversarial-verify", "implement the spec"]);
        assert!(cli.av);
    }

    #[test]
    fn parse_av_with_all_options() {
        let cli = parse(&[
            "--av",
            "--av-spec",
            "spec.md",
            "--av-threshold",
            "90",
            "--av-rounds",
            "5",
            "--av-model",
            "opus",
            "--av-prompt",
            "custom.txt",
            "--verify",
            "make ci",
            "implement the spec",
        ]);
        assert!(cli.av);
        assert_eq!(cli.av_spec.as_deref(), Some("spec.md"));
        assert_eq!(cli.av_threshold, Some(90));
        assert_eq!(cli.av_rounds, Some(5));
        assert_eq!(cli.av_model.as_deref(), Some("opus"));
        assert_eq!(cli.av_prompt.as_deref(), Some("custom.txt"));
        assert_eq!(cli.verify.as_deref(), Some("make ci"));
        assert_eq!(cli.prompt.as_deref(), Some("implement the spec"));
    }

    #[test]
    fn parse_av_threshold_invalid_falls_back() {
        let cli = parse(&["--av", "--av-threshold", "abc", "implement"]);
        assert!(cli.av);
        assert_eq!(cli.av_threshold, None); // invalid parse → None
    }

    #[test]
    fn av_banner_when_enabled() {
        let cli = parse(&[
            "--av",
            "--av-spec",
            "spec.md",
            "--av-threshold",
            "90",
            "implement",
        ]);
        let banner = cli.av_banner();
        assert!(banner.is_some());
        let (spec, threshold) = banner.unwrap();
        assert_eq!(spec, "spec.md");
        assert_eq!(threshold, 90);
    }

    #[test]
    fn av_banner_when_disabled() {
        let cli = parse(&["implement"]);
        assert!(cli.av_banner().is_none());
    }

    #[test]
    fn av_banner_defaults() {
        let cli = parse(&["--av", "implement"]);
        let (spec, threshold) = cli.av_banner().unwrap();
        assert_eq!(spec, "(auto-detect)");
        assert_eq!(threshold, 95);
    }

    // ─── Pipeline flag tests ───────────────────────────────────────

    #[test]
    fn parse_pipeline_flag() {
        let cli = parse(&["--pipeline", "pipeline.yaml"]);
        assert_eq!(cli.pipeline.as_deref(), Some("pipeline.yaml"));
        assert!(cli.prompt.is_none());
    }

    #[test]
    fn validate_pipeline_ok() {
        let cli = parse(&["--pipeline", "pipeline.yaml"]);
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn validate_pipeline_with_verify_fails() {
        let cli = parse(&["--pipeline", "p.yaml", "--verify", "make test"]);
        assert!(cli.validate().is_err());
    }

    #[test]
    fn validate_pipeline_with_av_fails() {
        let cli = parse(&["--pipeline", "p.yaml", "--av"]);
        assert!(cli.validate().is_err());
    }

    #[test]
    fn validate_pipeline_with_prompt_fails() {
        let cli = parse(&["--pipeline", "p.yaml", "some prompt"]);
        assert!(cli.validate().is_err());
    }

    #[test]
    fn validate_pipeline_with_resume_fails() {
        let cli = parse(&["--pipeline", "p.yaml", "--resume"]);
        assert!(cli.validate().is_err());
    }

    #[test]
    fn validate_pipeline_with_name_ok() {
        let cli = parse(&["--pipeline", "p.yaml", "--name", "my-session"]);
        assert!(cli.validate().is_ok());
        assert_eq!(cli.name.as_deref(), Some("my-session"));
    }
}
