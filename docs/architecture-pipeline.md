# Claude-Run Architecture: Multi-Agent Pipeline

## The Pattern We Keep Hitting

Every feature we add follows the same shape:

1. **A Claude instance does work** (implement, write tests, review, fix)
2. **Something checks the work** (shell command, another Claude instance, score parser)
3. **If it fails, loop back** with feedback

Today this is hardcoded as two specific loops:
- `run_with_retry` → rate-limit recovery loop (infrastructure)
- `run_verify_loop` → shell-command verify-fix loop (single pattern)

But the use cases keep multiplying:

| Use Case | Worker | Verifier | Loop Condition |
|----------|--------|----------|----------------|
| Implement + test | Claude #1 | `make test` | exit code 0 |
| Implement + spec review | Claude #1 | Claude #2 (scorer) | score ≥ threshold |
| Write tests for existing code | Claude #1 | `make test` (tests actually run) | exit code 0 |
| Implement then separate test-writer | Claude #1 (impl) | Claude #2 (test writer) → `make test` | exit code 0 |
| Security review | Claude #1 | Claude #2 (security auditor) | no critical findings |
| Multi-stage pipeline | Claude #1 → #2 → #3 | various | all stages pass |

We need an architecture that handles all of these without special-casing each one.

## Core Abstraction: Stages and Pipelines

### Stage

A **stage** is a unit of work that:
- Takes context (the filesystem state + a prompt/command)
- Produces a result (exit code, captured output, or structured verdict)
- Can be retried on rate-limit failures (infrastructure concern)

There are two kinds of stages:

```rust
enum Stage {
    /// A Claude instance doing work (implementing, reviewing, testing, etc.)
    Claude {
        role: String,           // human-readable role name
        prompt: String,         // what to tell this instance
        session_suffix: String, // appended to base session name
        model: Option<String>,  // override model
        capture_output: bool,   // capture stdout for verdict parsing
        extra_args: Vec<String>,
    },

    /// A shell command (deterministic verification)
    Shell {
        role: String,
        command: String,
    },
}
```

### StageResult

```rust
struct StageResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}
```

### Pipeline

A **pipeline** is an ordered sequence of stages with control flow between them.
The simplest pipeline is linear:

```
Stage A → Stage B → Stage C → done
```

But most real pipelines need **feedback loops**:

```
Stage A (implement) → Stage B (verify)
                          │
                     pass? → done
                     fail? → Stage A again (with feedback from B)
```

This is captured as:

```rust
struct Pipeline {
    stages: Vec<PipelineStep>,
}

enum PipelineStep {
    /// Run a stage once
    Run(Stage),

    /// Run a stage, then verify. Loop on failure.
    VerifyLoop {
        worker: Stage,
        verifier: Verifier,
        max_rounds: u32,
    },
}
```

### Verifier

A **verifier** checks a stage's work and produces a pass/fail decision.
This is the key abstraction — it unifies shell-command verification,
adversarial review, and spec-compliance scoring:

```rust
enum Verifier {
    /// Run a shell command. Pass if exit code 0.
    Shell {
        command: String,
    },

    /// Run another Claude instance. Parse its output for a verdict.
    Claude {
        stage: Stage,             // the reviewer stage config
        verdict_parser: VerdictParser,
    },

    /// Run verifiers in sequence. All must pass.
    Chain(Vec<Verifier>),
}

enum VerdictParser {
    /// Parse <verdict>SCORE: N\n...</verdict> blocks
    ScoreThreshold { threshold: u32 },

    /// Parse <verdict>PASS</verdict> or <verdict>FAIL\n...</verdict>
    PassFail,

    /// Just check exit code (for Claude instances that exit non-zero on failure)
    ExitCode,
}
```

### Feedback

When a verifier fails, it produces **feedback** that gets sent back to the
worker:

```rust
struct VerifyFeedback {
    /// What the verifier reported
    summary: String,
    /// Structured data (if available)
    score: Option<u32>,
    missing: Vec<String>,
    partial: Vec<String>,
    incorrect: Vec<String>,
}
```

The orchestrator builds a fix prompt from this feedback and resumes the worker
session with `--continue`.

## How It Maps to the Current Code

```
CURRENT                          NEW ARCHITECTURE
───────                          ────────────────
ClaudeRunner::run_with_retry  →  StageRunner::run (handles retries for ANY stage)
verify::run_verify_loop       →  PipelineRunner::run_verify_loop (generic)
(hardcoded in lib.rs)         →  Pipeline definition (composable)
```

### The key files

```
src/
├── stage.rs          # Stage, StageResult, stage execution
├── verifier.rs       # Verifier, VerdictParser, VerifyFeedback
├── pipeline.rs       # Pipeline, PipelineStep, PipelineRunner
├── verdict.rs        # Verdict parsing logic (score, pass/fail)
├── prompts.rs        # Prompt templates for different roles
├── runner.rs          # CommandRunner trait (unchanged)
├── rate_limit.rs      # Backoff logic (unchanged, used by stage runner)
├── config.rs          # Config (extended with pipeline defaults)
├── cli.rs             # CLI parsing (builds a Pipeline from flags)
├── lib.rs             # Entry point: parse CLI → build pipeline → run
├── output.rs          # Terminal output (extended)
├── notify.rs          # Notifications (unchanged)
└── slugify.rs         # Session naming (unchanged)
```

## Pipeline Execution

### PipelineRunner

```rust
struct PipelineRunner<R: CommandRunner> {
    cmd: R,
    config: Config,
    base_session: String,
    extra_args: Vec<String>,
}

impl<R: CommandRunner> PipelineRunner<R> {
    async fn run(&self, pipeline: &Pipeline) -> PipelineOutcome {
        for step in &pipeline.stages {
            match step {
                PipelineStep::Run(stage) => {
                    self.run_stage(stage).await?;
                }
                PipelineStep::VerifyLoop { worker, verifier, max_rounds } => {
                    self.run_verify_loop(worker, verifier, *max_rounds).await?;
                }
            }
        }
        PipelineOutcome::Success
    }
}
```

### Stage Execution (with retry)

Every stage — whether Claude or shell — runs through the same retry wrapper:

```rust
async fn run_stage(&self, stage: &Stage) -> Result<StageResult, RunError> {
    match stage {
        Stage::Claude { .. } => {
            // Build args, run with rate-limit retry
            // This is the existing run_with_retry logic, extracted
            self.run_claude_with_retry(stage).await
        }
        Stage::Shell { command, .. } => {
            // Run shell command (no retry — deterministic commands
            // don't benefit from blind retry)
            self.cmd.run_shell(command).await
        }
    }
}
```

### Generic Verify Loop

```rust
async fn run_verify_loop(
    &self,
    worker: &Stage,
    verifier: &Verifier,
    max_rounds: u32,
) -> VerifyOutcome {
    // Initial worker run
    self.run_stage(worker).await?;

    for round in 1..=max_rounds {
        // Run verifier
        let feedback = self.run_verifier(verifier).await?;

        if feedback.passed {
            return VerifyOutcome::Passed { round, score: feedback.score };
        }

        if round == max_rounds {
            return VerifyOutcome::Exhausted {
                round,
                score: feedback.score,
            };
        }

        // Build fix prompt from feedback, resume worker
        let fix_prompt = build_fix_prompt(&feedback, worker);
        self.resume_stage(worker, &fix_prompt).await?;
    }
}
```

### Running a Verifier

```rust
async fn run_verifier(&self, verifier: &Verifier) -> Result<VerifyFeedback, RunError> {
    match verifier {
        Verifier::Shell { command } => {
            let result = self.cmd.run_shell(command).await?;
            Ok(VerifyFeedback {
                passed: result.exit_code == 0,
                summary: tail_output(&result, 200),
                score: None,
                ..Default::default()
            })
        }
        Verifier::Claude { stage, verdict_parser } => {
            let result = self.run_stage_capturing(stage).await?;
            let verdict = verdict_parser.parse(&result.stdout);
            Ok(verdict.into_feedback())
        }
        Verifier::Chain(verifiers) => {
            for v in verifiers {
                let feedback = self.run_verifier(v).await?;
                if !feedback.passed {
                    return Ok(feedback);
                }
            }
            Ok(VerifyFeedback { passed: true, ..Default::default() })
        }
    }
}
```

## CLI → Pipeline Translation

The CLI flags build a `Pipeline`. This keeps the CLI simple while the
architecture stays general:

### Simple cases (backward compatible)

```bash
# Just run Claude
claude-run "do something"
#   → Pipeline: [Run(Claude { prompt: "do something" })]

# Run + shell verify
claude-run --verify "make ci" "do something"
#   → Pipeline: [VerifyLoop {
#       worker: Claude { prompt: "do something" },
#       verifier: Shell { command: "make ci" },
#       max_rounds: 5,
#     }]
```

### Adversarial spec review

```bash
claude-run --av --av-spec spec.md --verify "make ci" "implement the spec"
#   → Pipeline: [VerifyLoop {
#       worker: Claude { prompt: "implement the spec" },
#       verifier: Chain([
#           Shell { command: "make ci" },
#           Claude {
#               stage: Claude { prompt: REVIEWER_PROMPT, capture: true },
#               verdict_parser: ScoreThreshold { threshold: 95 },
#           },
#       ]),
#       max_rounds: 3,
#     }]
```

### Future: multi-stage pipeline (YAML config)

For complex pipelines, a YAML config file replaces CLI flags:

```bash
claude-run --pipeline pipeline.yaml
```

```yaml
# pipeline.yaml
stages:
  - name: implement
    type: claude
    prompt: "Implement the spec in spec.md. Write production code only, no tests."

  - name: write-tests
    type: claude
    prompt: |
      Read the spec in spec.md and the implementation.
      Write comprehensive tests. Do not modify the implementation.
    session_suffix: "-tests"
    model: sonnet  # tests don't need the strongest model

  - name: verify-tests
    type: verify-loop
    worker: implement          # re-runs the implement stage on failure
    max_rounds: 3
    verifier:
      chain:
        - type: shell
          command: "make ci"
        - type: claude
          session_suffix: "-reviewer"
          model: opus
          verdict: score
          threshold: 95
          prompt: |
            You are a spec-compliance auditor. Read spec.md.
            Score the implementation 0-100.
            <verdict>SCORE: N\n...</verdict>
```

This is a future extension — the immediate implementation only needs CLI flags.

## Recovery Model

### Rate-limit recovery (per-stage, existing)

Every Claude stage gets the existing retry + backoff + daily-cap-poll behavior.
This is infrastructure — it applies uniformly to all Claude stages.

### Verification recovery (per-loop)

When a verifier fails, the worker is resumed with feedback. This is the
verify-fix loop that already exists, now generalized.

### Stage failure recovery

If a Claude stage fails with a non-rate-limit error (exit code != 0, not
rate-limited), the pipeline stops and reports the failure. No automatic retry
for logic errors — that's what the verify loop is for.

### Session continuity

Each Claude stage maintains its own session:

```
base session:  impl-spec
implement:     impl-spec
test-writer:   impl-spec-tests
reviewer:      impl-spec-av-1 (round 1), impl-spec-av-2 (round 2)
```

Workers are resumed with `--continue` when looping. Reviewers get fresh
sessions each round (they must evaluate the current state independently).

## Concrete Implementation Plan

### Phase 1: Extract stage abstraction (refactor)

No new features — restructure existing code:

1. Extract `Stage` and `StageResult` from `ClaudeRunner`
2. Extract `Verifier` from `verify.rs`
3. Create `PipelineRunner` that replaces the hardcoded flow in `lib.rs`
4. All existing tests pass, CLI behavior unchanged

### Phase 2: Adversarial verification (new feature)

Build on the stage abstraction:

1. Add `Claude` verifier variant with `ScoreThreshold` parser
2. Add `run_stage_capturing()` for stdout capture
3. Add `--av*` CLI flags
4. Verdict parsing (`verdict.rs`)
5. Reviewer prompt template (`prompts.rs`)
6. Chain verifier (shell + Claude)

### Phase 3: Multi-stage pipelines (future)

1. YAML pipeline definition parser
2. Named stage references (for "worker" in verify-loop)
3. Stage dependencies / ordering
4. Pipeline-level configuration

### Phase 4: Advanced patterns (future)

1. Parallel stages (multiple independent Claude instances)
2. Stage-specific model selection
3. Conditional stages (only run if previous stage produced certain output)
4. Pipeline composition (one pipeline can include another)

## What Changes, What Stays

### Stays the same
- `CommandRunner` trait — still the subprocess abstraction
- `rate_limit.rs` — backoff logic, used by stage runner
- `notify.rs` — notification at pipeline completion
- `slugify.rs` — session naming
- CLI backward compatibility — existing flags work exactly as before

### Changes
- `lib.rs` — builds a `Pipeline` from CLI, runs it via `PipelineRunner`
- `runner.rs` — `ClaudeRunner` becomes `PipelineRunner`, gains `run_stage()`
- `verify.rs` — becomes a specific `Verifier` implementation, loop logic moves to `PipelineRunner`
- `config.rs` — extended with adversarial defaults
- `cli.rs` — new flags, builds pipeline
- `output.rs` — extended with adversarial/pipeline output

### New
- `stage.rs` — `Stage`, `StageResult`
- `verifier.rs` — `Verifier`, `VerdictParser`, `VerifyFeedback`
- `pipeline.rs` — `Pipeline`, `PipelineStep`, `PipelineRunner`
- `verdict.rs` — verdict parsing
- `prompts.rs` — reviewer prompt templates

## Why This Architecture

### It captures what we actually do

Every use case is: **run Claude → check result → loop if needed**. The
architecture makes this explicit instead of hardcoding it per-feature.

### It's additive, not rewrite

Phase 1 is a pure refactor. The existing behavior is preserved. New features
are new `Verifier` variants and new CLI flags that build different pipelines.

### The simple case stays simple

`claude-run "do something"` still works. It just builds a one-step pipeline
under the hood. No YAML required for the common case.

### Testing stays clean

The `CommandRunner` trait still works. Mock it, sequence the results, test
any pipeline shape without real processes.
