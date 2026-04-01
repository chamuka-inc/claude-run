/// A declarative description of a unit of work in a pipeline.
///
/// Stages don't execute themselves — the `PipelineRunner` interprets them.
#[derive(Debug, Clone)]
pub enum Stage {
    /// A Claude Code instance doing work (implementing, reviewing, testing, etc.)
    Claude {
        role: String,
        prompt: String,
        session_suffix: String,
        model: Option<String>,
        capture_output: bool,
        extra_args: Vec<String>,
    },

    /// A shell command (deterministic verification, build, etc.)
    Shell { role: String, command: String },
}

impl Stage {
    /// Create a Claude worker stage with standard defaults.
    pub fn claude_worker(prompt: impl Into<String>) -> Self {
        Self::Claude {
            role: "worker".into(),
            prompt: prompt.into(),
            session_suffix: String::new(),
            model: None,
            capture_output: false,
            extra_args: Vec::new(),
        }
    }

    /// Create a Claude reviewer stage that captures output for verdict parsing.
    pub fn claude_reviewer(
        prompt: impl Into<String>,
        session_suffix: impl Into<String>,
        model: Option<String>,
    ) -> Self {
        Self::Claude {
            role: "reviewer".into(),
            prompt: prompt.into(),
            session_suffix: session_suffix.into(),
            model,
            capture_output: true,
            extra_args: Vec::new(),
        }
    }

    /// Create a shell verification stage.
    pub fn shell(command: impl Into<String>) -> Self {
        Self::Shell {
            role: "verify".into(),
            command: command.into(),
        }
    }

    /// Human-readable role name for output.
    pub fn role(&self) -> &str {
        match self {
            Self::Claude { role, .. } | Self::Shell { role, .. } => role,
        }
    }
}

/// The result of executing a stage.
#[derive(Debug, Clone)]
pub struct StageResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_worker_defaults() {
        let stage = Stage::claude_worker("do something");
        match &stage {
            Stage::Claude {
                role,
                prompt,
                session_suffix,
                model,
                capture_output,
                extra_args,
            } => {
                assert_eq!(role, "worker");
                assert_eq!(prompt, "do something");
                assert!(session_suffix.is_empty());
                assert!(model.is_none());
                assert!(!capture_output);
                assert!(extra_args.is_empty());
            }
            _ => panic!("expected Claude variant"),
        }
        assert_eq!(stage.role(), "worker");
    }

    #[test]
    fn claude_reviewer_captures() {
        let stage = Stage::claude_reviewer("review this", "-av-1", Some("opus".into()));
        match &stage {
            Stage::Claude {
                capture_output,
                model,
                session_suffix,
                ..
            } => {
                assert!(capture_output);
                assert_eq!(model.as_deref(), Some("opus"));
                assert_eq!(session_suffix, "-av-1");
            }
            _ => panic!("expected Claude variant"),
        }
    }

    #[test]
    fn shell_stage() {
        let stage = Stage::shell("make test");
        match &stage {
            Stage::Shell { role, command } => {
                assert_eq!(role, "verify");
                assert_eq!(command, "make test");
            }
            _ => panic!("expected Shell variant"),
        }
    }
}
