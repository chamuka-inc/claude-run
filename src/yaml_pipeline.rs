use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::pipeline::{Pipeline, PipelineStep};
use crate::stage::Stage;
use crate::verifier::{VerdictParser, Verifier};

// ─── Error type ────────────────────────────────────────────────────

/// Errors encountered while loading or validating a YAML pipeline.
#[derive(Debug)]
pub struct YamlError {
    pub messages: Vec<String>,
}

impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for msg in &self.messages {
            writeln!(f, "  {msg}")?;
        }
        Ok(())
    }
}

impl std::error::Error for YamlError {}

// ─── Public API ────────────────────────────────────────────────────

/// Load a pipeline from a YAML file.
pub fn load_pipeline(path: &Path) -> Result<Pipeline, YamlError> {
    let contents = std::fs::read_to_string(path).map_err(|e| YamlError {
        messages: vec![format!("Cannot read {}: {e}", path.display())],
    })?;
    parse_pipeline(&contents)
}

/// Parse a pipeline from a YAML string.
pub fn parse_pipeline(yaml: &str) -> Result<Pipeline, YamlError> {
    let raw: RawPipeline = serde_yaml::from_str(yaml).map_err(|e| YamlError {
        messages: vec![format!("YAML syntax error: {e}")],
    })?;
    resolve(raw)
}

// ─── Raw (serde-facing) types ──────────────────────────────────────

#[derive(Deserialize)]
struct RawPipeline {
    stages: Vec<RawStage>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum RawStage {
    #[serde(rename = "claude")]
    Claude {
        name: String,
        prompt: Option<String>,
        prompt_file: Option<String>,
        session_suffix: Option<String>,
        model: Option<String>,
        #[serde(default)]
        capture_output: bool,
        #[serde(default)]
        extra_args: Vec<String>,
    },

    #[serde(rename = "shell")]
    Shell { name: String, command: String },

    #[serde(rename = "verify-loop")]
    VerifyLoop {
        name: String,
        worker: String,
        max_rounds: Option<u32>,
        verifier: RawVerifier,
    },
}

impl RawStage {
    fn name(&self) -> &str {
        match self {
            Self::Claude { name, .. }
            | Self::Shell { name, .. }
            | Self::VerifyLoop { name, .. } => name,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawVerifier {
    Chain { chain: Vec<RawVerifierItem> },
    Single(RawVerifierItem),
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum RawVerifierItem {
    #[serde(rename = "shell")]
    Shell { command: String },

    #[serde(rename = "claude")]
    Claude {
        prompt: Option<String>,
        prompt_file: Option<String>,
        session_suffix: Option<String>,
        model: Option<String>,
        #[serde(default)]
        verdict: VerdictType,
        threshold: Option<u32>,
    },
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "lowercase")]
enum VerdictType {
    Score,
    #[default]
    Passfail,
    Exitcode,
}

// ─── Resolution (second pass) ──────────────────────────────────────

fn resolve(raw: RawPipeline) -> Result<Pipeline, YamlError> {
    let mut errors = Vec::new();
    let mut named_stages: HashMap<String, Stage> = HashMap::new();
    let mut steps: Vec<PipelineStep> = Vec::new();

    // Check for duplicate names
    let mut seen_names: HashMap<String, usize> = HashMap::new();
    for (i, stage) in raw.stages.iter().enumerate() {
        let name = stage.name().to_string();
        if let Some(prev) = seen_names.insert(name.clone(), i) {
            errors.push(format!(
                "Duplicate stage name '{}' (stages {} and {})",
                name,
                prev + 1,
                i + 1
            ));
        }
    }

    for raw_stage in &raw.stages {
        match raw_stage {
            RawStage::Claude {
                name,
                prompt,
                prompt_file,
                session_suffix,
                model,
                capture_output,
                extra_args,
            } => {
                let resolved_prompt = resolve_prompt(prompt, prompt_file, name, &mut errors);
                let stage = Stage::Claude {
                    role: name.clone(),
                    prompt: resolved_prompt,
                    session_suffix: session_suffix.clone().unwrap_or_default(),
                    model: model.clone(),
                    capture_output: *capture_output,
                    extra_args: extra_args.clone(),
                };
                named_stages.insert(name.clone(), stage.clone());
                steps.push(PipelineStep::Run(stage));
            }

            RawStage::Shell { name, command } => {
                let stage = Stage::Shell {
                    role: name.clone(),
                    command: command.clone(),
                };
                named_stages.insert(name.clone(), stage.clone());
                steps.push(PipelineStep::Run(stage));
            }

            RawStage::VerifyLoop {
                name,
                worker,
                max_rounds,
                verifier,
            } => {
                let worker_stage = match named_stages.get(worker) {
                    Some(s) => s.clone(),
                    None => {
                        errors.push(format!(
                            "Stage '{name}': worker '{worker}' not found (must be defined earlier)"
                        ));
                        continue;
                    }
                };

                let resolved_verifier = resolve_verifier(verifier, name, &mut errors);

                // Remove the earlier standalone Run step for this worker —
                // the verify-loop replaces it
                steps.retain(|step| {
                    if let PipelineStep::Run(s) = step {
                        s.role() != worker.as_str()
                    } else {
                        true
                    }
                });

                steps.push(PipelineStep::VerifyLoop {
                    worker: worker_stage,
                    verifier: resolved_verifier,
                    max_rounds: max_rounds.unwrap_or(3),
                });
            }
        }
    }

    if !errors.is_empty() {
        return Err(YamlError { messages: errors });
    }

    Ok(Pipeline { steps })
}

fn resolve_prompt(
    prompt: &Option<String>,
    prompt_file: &Option<String>,
    stage_name: &str,
    errors: &mut Vec<String>,
) -> String {
    match (prompt, prompt_file) {
        (Some(p), None) => p.clone(),
        (None, Some(path)) => match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) => {
                errors.push(format!(
                    "Stage '{stage_name}': cannot read prompt_file '{path}': {e}"
                ));
                String::new()
            }
        },
        (Some(_), Some(_)) => {
            errors.push(format!(
                "Stage '{stage_name}': cannot specify both 'prompt' and 'prompt_file'"
            ));
            String::new()
        }
        (None, None) => {
            errors.push(format!(
                "Stage '{stage_name}': must specify 'prompt' or 'prompt_file'"
            ));
            String::new()
        }
    }
}

fn resolve_verifier(raw: &RawVerifier, stage_name: &str, errors: &mut Vec<String>) -> Verifier {
    match raw {
        RawVerifier::Chain { chain } => {
            let verifiers: Vec<Verifier> = chain
                .iter()
                .map(|item| resolve_verifier_item(item, stage_name, errors))
                .collect();
            if verifiers.len() == 1 {
                verifiers.into_iter().next().unwrap()
            } else {
                Verifier::Chain(verifiers)
            }
        }
        RawVerifier::Single(item) => resolve_verifier_item(item, stage_name, errors),
    }
}

fn resolve_verifier_item(
    item: &RawVerifierItem,
    stage_name: &str,
    errors: &mut Vec<String>,
) -> Verifier {
    match item {
        RawVerifierItem::Shell { command } => Verifier::Shell {
            command: command.clone(),
        },
        RawVerifierItem::Claude {
            prompt,
            prompt_file,
            session_suffix,
            model,
            verdict,
            threshold,
        } => {
            let resolved_prompt = resolve_prompt(
                prompt,
                prompt_file,
                &format!("{stage_name}/verifier"),
                errors,
            );

            let verdict_parser = match verdict {
                VerdictType::Score => {
                    let t = match threshold {
                        Some(t) => *t,
                        None => {
                            errors.push(format!(
                                "Stage '{stage_name}': verdict 'score' requires a 'threshold' field"
                            ));
                            95
                        }
                    };
                    VerdictParser::ScoreThreshold { threshold: t }
                }
                VerdictType::Passfail => VerdictParser::PassFail,
                VerdictType::Exitcode => VerdictParser::ExitCode,
            };

            let reviewer_stage = Stage::Claude {
                role: format!("{stage_name}-reviewer"),
                prompt: resolved_prompt,
                session_suffix: session_suffix.clone().unwrap_or_else(|| "-reviewer".into()),
                model: model.clone(),
                capture_output: true,
                extra_args: Vec::new(),
            };

            Verifier::Claude {
                stage: reviewer_stage,
                verdict_parser,
            }
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Happy path ────────────────────────────────────────────────

    #[test]
    fn parse_single_claude_stage() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Implement the feature"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        assert_eq!(pipeline.steps.len(), 1);
        match &pipeline.steps[0] {
            PipelineStep::Run(Stage::Claude { role, prompt, .. }) => {
                assert_eq!(role, "implement");
                assert_eq!(prompt, "Implement the feature");
            }
            other => panic!("expected Run(Claude), got {other:?}"),
        }
    }

    #[test]
    fn parse_shell_stage() {
        let yaml = r#"
stages:
  - name: build
    type: shell
    command: "make build"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        assert_eq!(pipeline.steps.len(), 1);
        match &pipeline.steps[0] {
            PipelineStep::Run(Stage::Shell { role, command }) => {
                assert_eq!(role, "build");
                assert_eq!(command, "make build");
            }
            other => panic!("expected Run(Shell), got {other:?}"),
        }
    }

    #[test]
    fn parse_multi_stage_pipeline() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Implement the spec"
  - name: write-tests
    type: claude
    prompt: "Write tests"
    session_suffix: "-tests"
    model: sonnet
  - name: build
    type: shell
    command: "make build"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        assert_eq!(pipeline.steps.len(), 3);
    }

    #[test]
    fn parse_verify_loop_with_shell_verifier() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Implement the spec"
  - name: verify
    type: verify-loop
    worker: implement
    max_rounds: 5
    verifier:
      type: shell
      command: "make test"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        // The standalone Run(implement) should be replaced by the VerifyLoop
        assert_eq!(pipeline.steps.len(), 1);
        match &pipeline.steps[0] {
            PipelineStep::VerifyLoop {
                worker,
                verifier,
                max_rounds,
            } => {
                assert_eq!(worker.role(), "implement");
                assert_eq!(*max_rounds, 5);
                match verifier {
                    Verifier::Shell { command } => assert_eq!(command, "make test"),
                    other => panic!("expected Shell verifier, got {other:?}"),
                }
            }
            other => panic!("expected VerifyLoop, got {other:?}"),
        }
    }

    #[test]
    fn parse_verify_loop_with_claude_verifier() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Implement the spec"
  - name: review
    type: verify-loop
    worker: implement
    verifier:
      type: claude
      prompt: "Score the implementation 0-100"
      verdict: score
      threshold: 95
      model: opus
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        assert_eq!(pipeline.steps.len(), 1);
        match &pipeline.steps[0] {
            PipelineStep::VerifyLoop { verifier, .. } => match verifier {
                Verifier::Claude {
                    stage,
                    verdict_parser,
                } => {
                    assert!(matches!(
                        stage,
                        Stage::Claude {
                            capture_output: true,
                            ..
                        }
                    ));
                    assert!(matches!(
                        verdict_parser,
                        VerdictParser::ScoreThreshold { threshold: 95 }
                    ));
                }
                other => panic!("expected Claude verifier, got {other:?}"),
            },
            other => panic!("expected VerifyLoop, got {other:?}"),
        }
    }

    #[test]
    fn parse_verify_loop_with_chain() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Implement the spec"
  - name: verify
    type: verify-loop
    worker: implement
    max_rounds: 3
    verifier:
      chain:
        - type: shell
          command: "make ci"
        - type: claude
          prompt: "Score 0-100"
          verdict: score
          threshold: 90
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        assert_eq!(pipeline.steps.len(), 1);
        match &pipeline.steps[0] {
            PipelineStep::VerifyLoop { verifier, .. } => match verifier {
                Verifier::Chain(verifiers) => {
                    assert_eq!(verifiers.len(), 2);
                    assert!(matches!(&verifiers[0], Verifier::Shell { .. }));
                    assert!(matches!(&verifiers[1], Verifier::Claude { .. }));
                }
                other => panic!("expected Chain, got {other:?}"),
            },
            other => panic!("expected VerifyLoop, got {other:?}"),
        }
    }

    #[test]
    fn parse_all_optional_fields() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Do it"
    session_suffix: "-impl"
    model: opus
    capture_output: true
    extra_args: ["--max-turns", "100"]
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        match &pipeline.steps[0] {
            PipelineStep::Run(Stage::Claude {
                session_suffix,
                model,
                capture_output,
                extra_args,
                ..
            }) => {
                assert_eq!(session_suffix, "-impl");
                assert_eq!(model.as_deref(), Some("opus"));
                assert!(capture_output);
                assert_eq!(extra_args, &["--max-turns", "100"]);
            }
            other => panic!("expected Claude with all fields, got {other:?}"),
        }
    }

    #[test]
    fn parse_defaults_when_omitted() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Do it"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        match &pipeline.steps[0] {
            PipelineStep::Run(Stage::Claude {
                session_suffix,
                model,
                capture_output,
                extra_args,
                ..
            }) => {
                assert!(session_suffix.is_empty());
                assert!(model.is_none());
                assert!(!capture_output);
                assert!(extra_args.is_empty());
            }
            other => panic!("expected Claude with defaults, got {other:?}"),
        }
    }

    #[test]
    fn verify_loop_default_max_rounds() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Do it"
  - name: verify
    type: verify-loop
    worker: implement
    verifier:
      type: shell
      command: "make test"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        match &pipeline.steps[0] {
            PipelineStep::VerifyLoop { max_rounds, .. } => {
                assert_eq!(*max_rounds, 3);
            }
            other => panic!("expected VerifyLoop, got {other:?}"),
        }
    }

    #[test]
    fn verify_loop_replaces_earlier_run_step() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Implement"
  - name: write-tests
    type: claude
    prompt: "Write tests"
  - name: verify
    type: verify-loop
    worker: implement
    verifier:
      type: shell
      command: "make test"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        // Should have: write-tests (Run) + verify (VerifyLoop)
        // The standalone implement Run should be gone
        assert_eq!(pipeline.steps.len(), 2);
        assert!(
            matches!(&pipeline.steps[0], PipelineStep::Run(Stage::Claude { role, .. }) if role == "write-tests")
        );
        assert!(matches!(
            &pipeline.steps[1],
            PipelineStep::VerifyLoop { .. }
        ));
    }

    #[test]
    fn chain_with_single_item_unwraps() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Do it"
  - name: verify
    type: verify-loop
    worker: implement
    verifier:
      chain:
        - type: shell
          command: "make test"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        match &pipeline.steps[0] {
            PipelineStep::VerifyLoop { verifier, .. } => {
                // Single-item chain should unwrap to just the verifier
                assert!(matches!(verifier, Verifier::Shell { .. }));
            }
            other => panic!("expected VerifyLoop, got {other:?}"),
        }
    }

    // ─── Error cases ───────────────────────────────────────────────

    #[test]
    fn error_missing_prompt() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err
            .messages
            .iter()
            .any(|m| m.contains("must specify 'prompt'")));
    }

    #[test]
    fn error_both_prompt_and_prompt_file() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "inline"
    prompt_file: "file.md"
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err
            .messages
            .iter()
            .any(|m| m.contains("cannot specify both")));
    }

    #[test]
    fn error_unknown_worker_reference() {
        let yaml = r#"
stages:
  - name: verify
    type: verify-loop
    worker: nonexistent
    verifier:
      type: shell
      command: "make test"
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err
            .messages
            .iter()
            .any(|m| m.contains("'nonexistent' not found")));
    }

    #[test]
    fn error_forward_reference() {
        let yaml = r#"
stages:
  - name: verify
    type: verify-loop
    worker: implement
    verifier:
      type: shell
      command: "make test"
  - name: implement
    type: claude
    prompt: "Do it"
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err
            .messages
            .iter()
            .any(|m| m.contains("'implement' not found")));
    }

    #[test]
    fn error_duplicate_stage_names() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "First"
  - name: implement
    type: claude
    prompt: "Second"
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err
            .messages
            .iter()
            .any(|m| m.contains("Duplicate stage name")));
    }

    #[test]
    fn error_invalid_yaml_syntax() {
        let yaml = "stages:\n  - name: [invalid";
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err.messages.iter().any(|m| m.contains("YAML syntax error")));
    }

    #[test]
    fn error_unknown_stage_type() {
        let yaml = r#"
stages:
  - name: foo
    type: unknown-type
    prompt: "bar"
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(!err.messages.is_empty());
    }

    #[test]
    fn error_score_verdict_without_threshold() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Do it"
  - name: review
    type: verify-loop
    worker: implement
    verifier:
      type: claude
      prompt: "Score it"
      verdict: score
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err
            .messages
            .iter()
            .any(|m| m.contains("requires a 'threshold'")));
    }

    #[test]
    fn error_prompt_file_not_found() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt_file: "/nonexistent/path/to/prompt.md"
"#;
        let err = parse_pipeline(yaml).unwrap_err();
        assert!(err
            .messages
            .iter()
            .any(|m| m.contains("cannot read prompt_file")));
    }

    #[test]
    fn passfail_verdict_default() {
        let yaml = r#"
stages:
  - name: implement
    type: claude
    prompt: "Do it"
  - name: review
    type: verify-loop
    worker: implement
    verifier:
      type: claude
      prompt: "Review it"
"#;
        let pipeline = parse_pipeline(yaml).unwrap();
        match &pipeline.steps[0] {
            PipelineStep::VerifyLoop { verifier, .. } => match verifier {
                Verifier::Claude { verdict_parser, .. } => {
                    assert!(matches!(verdict_parser, VerdictParser::PassFail));
                }
                other => panic!("expected Claude verifier, got {other:?}"),
            },
            other => panic!("expected VerifyLoop, got {other:?}"),
        }
    }
}
