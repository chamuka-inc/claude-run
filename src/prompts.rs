use crate::verifier::VerifyFeedback;

/// Default reviewer system prompt for adversarial spec-compliance review.
const REVIEWER_PROMPT_TEMPLATE: &str = r#"You are a strict spec-compliance auditor. Your job is to score how completely
and faithfully a specification has been implemented. You are not here to be
helpful or encouraging — you are here to find gaps.

## The Specification
Read the spec file: {spec_file}

## Your Audit Process
1. Read the spec file completely. List every discrete requirement (functional
   requirements, edge cases, error handling, configuration options, API
   contracts, data formats, validation rules, etc.)
2. For each requirement, check whether it is implemented by reading the
   relevant source files
3. Score the implementation

## Scoring Rules
- Start at 100
- For each requirement that is completely missing: -10 to -20 depending on
  importance
- For each requirement that is partially implemented (stubbed, TODO, happy
  path only): -5 to -10
- For each requirement that is implemented but incorrectly: -5 to -15
- Minimum score is 0

## Output Format
You MUST end your response with a verdict block in exactly this format:

<verdict>
SCORE: {number}

MISSING:
- [file:line] requirement X from spec section Y is not implemented
- [file:line] requirement Z is stubbed with a TODO

PARTIAL:
- [file:line] requirement A only handles the happy path, spec requires error handling for ...
- [file:line] requirement B is implemented but missing the edge case where ...

INCORRECT:
- [file:line] requirement C is implemented but does X when spec says Y
</verdict>

Be specific. Cite the spec section and the source file. If everything is
fully implemented, output SCORE: 100 with empty sections."#;

/// Build the reviewer prompt for adversarial verification.
pub fn build_review_prompt(original_prompt: &str, spec_file: Option<&str>) -> String {
    let spec_reference = match spec_file {
        Some(path) => path.to_string(),
        None => format!(
            "The spec is referenced in the original task prompt. \
             The developer was asked to: \"{original_prompt}\". \
             Find and read the spec file."
        ),
    };

    REVIEWER_PROMPT_TEMPLATE.replace("{spec_file}", &spec_reference)
}

/// Build the fix prompt sent to the worker after a failed adversarial review.
pub fn build_av_fix_prompt(feedback: &VerifyFeedback) -> String {
    let score = feedback.score.unwrap_or(0);

    let mut prompt = format!(
        "A spec-compliance audit scored your implementation {score}/100.\n\
         You need to address these gaps:\n"
    );

    if !feedback.missing.is_empty() {
        prompt.push_str("\n## Missing (not implemented)\n");
        for item in &feedback.missing {
            prompt.push_str(&format!("- {item}\n"));
        }
    }

    if !feedback.partial.is_empty() {
        prompt.push_str("\n## Partial (incomplete implementation)\n");
        for item in &feedback.partial {
            prompt.push_str(&format!("- {item}\n"));
        }
    }

    if !feedback.incorrect.is_empty() {
        prompt.push_str("\n## Incorrect (wrong behavior)\n");
        for item in &feedback.incorrect {
            prompt.push_str(&format!("- {item}\n"));
        }
    }

    prompt.push_str(
        "\nGo through each item and implement it fully. Do not skip any.\n\
         Do not add TODO comments — write the actual implementation.",
    );

    prompt
}

/// Build the fix prompt with an explicit threshold value.
pub fn build_av_fix_prompt_with_threshold(feedback: &VerifyFeedback, threshold: u32) -> String {
    let score = feedback.score.unwrap_or(0);

    let mut prompt = format!(
        "A spec-compliance audit scored your implementation {score}/100.\n\
         The threshold is {threshold}. You need to address these gaps:\n"
    );

    if !feedback.missing.is_empty() {
        prompt.push_str("\n## Missing (not implemented)\n");
        for item in &feedback.missing {
            prompt.push_str(&format!("- {item}\n"));
        }
    }

    if !feedback.partial.is_empty() {
        prompt.push_str("\n## Partial (incomplete implementation)\n");
        for item in &feedback.partial {
            prompt.push_str(&format!("- {item}\n"));
        }
    }

    if !feedback.incorrect.is_empty() {
        prompt.push_str("\n## Incorrect (wrong behavior)\n");
        for item in &feedback.incorrect {
            prompt.push_str(&format!("- {item}\n"));
        }
    }

    prompt.push_str(
        "\nGo through each item and implement it fully. Do not skip any.\n\
         Do not add TODO comments — write the actual implementation.",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_prompt_with_spec_file() {
        let prompt = build_review_prompt("implement the spec", Some("spec.md"));
        assert!(prompt.contains("Read the spec file: spec.md"));
        assert!(prompt.contains("SCORE:"));
        assert!(prompt.contains("MISSING:"));
    }

    #[test]
    fn review_prompt_without_spec_file() {
        let prompt = build_review_prompt("implement the spec in docs/spec.md", None);
        assert!(prompt.contains("implement the spec in docs/spec.md"));
        assert!(prompt.contains("Find and read the spec file"));
    }

    #[test]
    fn fix_prompt_with_all_sections() {
        let feedback = VerifyFeedback {
            passed: false,
            summary: String::new(),
            score: Some(72),
            missing: vec!["OAuth flow".into()],
            partial: vec!["error handling".into()],
            incorrect: vec!["email validation".into()],
        };
        let prompt = build_av_fix_prompt_with_threshold(&feedback, 95);
        assert!(prompt.contains("72/100"));
        assert!(prompt.contains("threshold is 95"));
        assert!(prompt.contains("## Missing"));
        assert!(prompt.contains("OAuth flow"));
        assert!(prompt.contains("## Partial"));
        assert!(prompt.contains("error handling"));
        assert!(prompt.contains("## Incorrect"));
        assert!(prompt.contains("email validation"));
        assert!(prompt.contains("Do not add TODO comments"));
    }

    #[test]
    fn fix_prompt_with_only_missing() {
        let feedback = VerifyFeedback {
            passed: false,
            score: Some(60),
            missing: vec!["feature A".into(), "feature B".into()],
            ..Default::default()
        };
        let prompt = build_av_fix_prompt(&feedback);
        assert!(prompt.contains("60/100"));
        assert!(prompt.contains("feature A"));
        assert!(prompt.contains("feature B"));
        assert!(!prompt.contains("## Partial"));
        assert!(!prompt.contains("## Incorrect"));
    }
}
