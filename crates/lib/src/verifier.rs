use crate::stage::Stage;

/// How a verifier checks work and decides pass/fail.
#[derive(Debug, Clone)]
pub enum Verifier {
    /// Run a shell command. Pass if exit code 0.
    Shell { command: String },

    /// Run another Claude instance. Parse its output for a verdict.
    Claude {
        stage: Stage,
        verdict_parser: VerdictParser,
    },

    /// Run verifiers in sequence. All must pass (short-circuits on first failure).
    Chain(Vec<Verifier>),
}

/// How to parse a verifier's output into a pass/fail decision.
#[derive(Debug, Clone)]
pub enum VerdictParser {
    /// Parse `<verdict>SCORE: N\n...</verdict>` blocks. Pass if score >= threshold.
    ScoreThreshold { threshold: u32 },

    /// Parse `<verdict>PASS</verdict>` or `<verdict>FAIL\n...</verdict>`.
    PassFail,

    /// Just check exit code (exit 0 = pass).
    ExitCode,
}

/// Feedback produced by a verifier after checking work.
#[derive(Debug, Clone, Default)]
pub struct VerifyFeedback {
    pub passed: bool,
    pub summary: String,
    pub score: Option<u32>,
    pub missing: Vec<String>,
    pub partial: Vec<String>,
    pub incorrect: Vec<String>,
}

impl VerifyFeedback {
    /// Create a simple pass/fail feedback from a shell command result.
    pub fn from_shell(exit_code: i32, output: &str) -> Self {
        Self {
            passed: exit_code == 0,
            summary: tail_lines(output, 200),
            ..Default::default()
        }
    }
}

/// Take the last N lines of a string.
pub fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_feedback_pass() {
        let fb = VerifyFeedback::from_shell(0, "all tests passed");
        assert!(fb.passed);
        assert_eq!(fb.summary, "all tests passed");
        assert!(fb.score.is_none());
    }

    #[test]
    fn shell_feedback_fail() {
        let fb = VerifyFeedback::from_shell(1, "test failed\nassert_eq failed");
        assert!(!fb.passed);
        assert!(fb.summary.contains("assert_eq failed"));
    }

    #[test]
    fn tail_lines_truncates() {
        let long = (0..500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_lines(&long, 200);
        assert_eq!(tail.lines().count(), 200);
        assert!(tail.contains("line 499"));
        assert!(!tail.contains("line 0\n"));
    }

    #[test]
    fn tail_lines_short_input() {
        let short = "a\nb\nc";
        assert_eq!(tail_lines(short, 200), short);
    }
}
