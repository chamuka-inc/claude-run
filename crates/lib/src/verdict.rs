use crate::verifier::{VerdictParser, VerifyFeedback};

/// Parsed score and itemized findings from a reviewer's verdict block.
#[derive(Debug, Clone)]
pub struct ReviewScore {
    pub score: u32,
    pub missing: Vec<String>,
    pub partial: Vec<String>,
    pub incorrect: Vec<String>,
}

/// Result of attempting to parse a verdict from reviewer output.
#[derive(Debug, Clone)]
pub enum ReviewVerdict {
    Scored(ReviewScore),
    NoVerdict,
}

/// Parse reviewer stdout into `VerifyFeedback` using the given verdict parser.
pub fn parse_to_feedback(stdout: &str, parser: &VerdictParser) -> VerifyFeedback {
    match parser {
        VerdictParser::ScoreThreshold { threshold } => {
            let verdict = parse_verdict(stdout);
            match verdict {
                ReviewVerdict::Scored(score) => VerifyFeedback {
                    passed: score.score >= *threshold,
                    summary: format!("Score: {}/100 (threshold: {})", score.score, threshold),
                    score: Some(score.score),
                    missing: score.missing.clone(),
                    partial: score.partial.clone(),
                    incorrect: score.incorrect.clone(),
                },
                ReviewVerdict::NoVerdict => VerifyFeedback {
                    passed: false,
                    summary: "Reviewer did not produce a parseable verdict".into(),
                    score: Some(0),
                    missing: vec!["Reviewer did not produce a verdict".into()],
                    ..Default::default()
                },
            }
        }
        VerdictParser::PassFail => {
            let passed = parse_pass_fail(stdout);
            VerifyFeedback {
                passed,
                summary: if passed { "PASS".into() } else { "FAIL".into() },
                ..Default::default()
            }
        }
        VerdictParser::ExitCode => {
            // Exit code is handled at the stage level, not here.
            // If we get here, the stage succeeded (exit 0), so pass.
            VerifyFeedback {
                passed: true,
                ..Default::default()
            }
        }
    }
}

/// Parse the last `<verdict>...</verdict>` block from reviewer output.
pub fn parse_verdict(stdout: &str) -> ReviewVerdict {
    let Some(block) = extract_last_verdict_block(stdout) else {
        return ReviewVerdict::NoVerdict;
    };

    let score = parse_score_line(&block);
    let Some(score) = score else {
        return ReviewVerdict::NoVerdict;
    };

    ReviewVerdict::Scored(ReviewScore {
        score: score.min(100),
        missing: parse_section(&block, "MISSING:"),
        partial: parse_section(&block, "PARTIAL:"),
        incorrect: parse_section(&block, "INCORRECT:"),
    })
}

/// Parse `<verdict>PASS</verdict>` or `<verdict>FAIL...</verdict>`.
fn parse_pass_fail(stdout: &str) -> bool {
    let Some(block) = extract_last_verdict_block(stdout) else {
        return false;
    };
    block.trim().starts_with("PASS")
}

/// Extract the content of the last `<verdict>...</verdict>` block.
fn extract_last_verdict_block(text: &str) -> Option<String> {
    let end_tag = "</verdict>";
    let start_tag = "<verdict>";

    let end_pos = text.rfind(end_tag)?;
    let search_region = &text[..end_pos];
    let start_pos = search_region.rfind(start_tag)?;

    let content_start = start_pos + start_tag.len();
    Some(text[content_start..end_pos].to_string())
}

/// Extract `SCORE: N` from a verdict block.
fn parse_score_line(block: &str) -> Option<u32> {
    for line in block.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("SCORE:") {
            if let Ok(n) = rest.trim().parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// Extract bullet items under a section header (e.g., "MISSING:").
fn parse_section(block: &str, header: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_section = false;

    for line in block.lines() {
        let trimmed = line.trim();

        if trimmed == header {
            in_section = true;
            continue;
        }

        // New section header ends the current section
        if in_section && !trimmed.is_empty() && !trimmed.starts_with('-') && trimmed.ends_with(':')
        {
            break;
        }

        if in_section {
            if let Some(item) = trimmed.strip_prefix("- ") {
                items.push(item.to_string());
            }
        }
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_verdict() {
        let output = r#"
Some reviewer analysis...

<verdict>
SCORE: 85

MISSING:
- [src/auth.rs] OAuth flow not implemented
- [src/api.rs] rate limiting endpoint missing

PARTIAL:
- [src/db.rs] migration only handles happy path

INCORRECT:
- [src/validate.rs] email regex rejects valid addresses
</verdict>
"#;
        match parse_verdict(output) {
            ReviewVerdict::Scored(score) => {
                assert_eq!(score.score, 85);
                assert_eq!(score.missing.len(), 2);
                assert_eq!(score.partial.len(), 1);
                assert_eq!(score.incorrect.len(), 1);
                assert!(score.missing[0].contains("OAuth"));
                assert!(score.partial[0].contains("happy path"));
                assert!(score.incorrect[0].contains("email regex"));
            }
            other => panic!("expected Scored, got {other:?}"),
        }
    }

    #[test]
    fn parse_perfect_score() {
        let output = "<verdict>\nSCORE: 100\n\nMISSING:\n\nPARTIAL:\n\nINCORRECT:\n</verdict>";
        match parse_verdict(output) {
            ReviewVerdict::Scored(score) => {
                assert_eq!(score.score, 100);
                assert!(score.missing.is_empty());
                assert!(score.partial.is_empty());
                assert!(score.incorrect.is_empty());
            }
            other => panic!("expected Scored, got {other:?}"),
        }
    }

    #[test]
    fn parse_score_capped_at_100() {
        let output = "<verdict>\nSCORE: 150\n</verdict>";
        match parse_verdict(output) {
            ReviewVerdict::Scored(score) => assert_eq!(score.score, 100),
            other => panic!("expected Scored, got {other:?}"),
        }
    }

    #[test]
    fn parse_no_verdict_block() {
        let output = "Just some text without any verdict";
        assert!(matches!(parse_verdict(output), ReviewVerdict::NoVerdict));
    }

    #[test]
    fn parse_malformed_score() {
        let output = "<verdict>\nSCORE: abc\n</verdict>";
        assert!(matches!(parse_verdict(output), ReviewVerdict::NoVerdict));
    }

    #[test]
    fn parse_uses_last_verdict_block() {
        let output =
            "<verdict>\nSCORE: 50\n</verdict>\nMore text\n<verdict>\nSCORE: 90\n</verdict>";
        match parse_verdict(output) {
            ReviewVerdict::Scored(score) => assert_eq!(score.score, 90),
            other => panic!("expected Scored with 90, got {other:?}"),
        }
    }

    #[test]
    fn parse_pass_fail_pass() {
        let output = "<verdict>PASS</verdict>";
        assert!(parse_pass_fail(output));
    }

    #[test]
    fn parse_pass_fail_fail() {
        let output = "<verdict>FAIL\nSome reason</verdict>";
        assert!(!parse_pass_fail(output));
    }

    #[test]
    fn parse_to_feedback_threshold_pass() {
        let output = "<verdict>\nSCORE: 96\n</verdict>";
        let parser = VerdictParser::ScoreThreshold { threshold: 95 };
        let fb = parse_to_feedback(output, &parser);
        assert!(fb.passed);
        assert_eq!(fb.score, Some(96));
    }

    #[test]
    fn parse_to_feedback_threshold_fail() {
        let output = "<verdict>\nSCORE: 80\n\nMISSING:\n- something missing\n</verdict>";
        let parser = VerdictParser::ScoreThreshold { threshold: 95 };
        let fb = parse_to_feedback(output, &parser);
        assert!(!fb.passed);
        assert_eq!(fb.score, Some(80));
        assert_eq!(fb.missing.len(), 1);
    }

    #[test]
    fn parse_to_feedback_no_verdict() {
        let output = "no verdict here";
        let parser = VerdictParser::ScoreThreshold { threshold: 95 };
        let fb = parse_to_feedback(output, &parser);
        assert!(!fb.passed);
        assert_eq!(fb.score, Some(0));
    }

    #[test]
    fn parse_score_zero() {
        let output = "<verdict>\nSCORE: 0\n\nMISSING:\n- everything\n</verdict>";
        match parse_verdict(output) {
            ReviewVerdict::Scored(score) => assert_eq!(score.score, 0),
            other => panic!("expected Scored, got {other:?}"),
        }
    }
}
