# Adversarial Verification Design

## Problem

When given a large specification file, Claude tends to implement the minimum it
can get away with — the happy path, the obvious features, the parts that are
easy. It skips edge cases, omits secondary requirements, stubs out complex
sections, and calls it done. Existing `--verify` only catches what tests cover,
and if Claude wrote those tests too, it's marking its own homework.

**Adversarial verification** launches a second, independent Claude process that
reads the spec, reads the implementation, and scores how faithfully the spec was
implemented. The loop retries until the score meets a threshold.

## Design Principles

1. **Spec is the source of truth** — the reviewer scores against the spec, not
   its own opinion of what good code looks like
2. **Numeric score, not vibes** — a concrete 0-100 rating with itemized
   deductions, not "looks good" or "needs work"
3. **Threshold-driven loop** — keep sending the worker back until the score
   meets the bar (default: 95)
4. **Independent context** — the reviewer gets a fresh session with no shared
   memory with the worker
5. **Combine, don't replace** — works alongside `--verify` (tests must pass
   before the reviewer even looks)

## CLI Interface

```
claude-run --adversarial-verify [OPTIONS] "implement the spec in spec.md"
claude-run --av --verify "make ci" "implement the spec in spec.md"
```

### New Flags

| Flag | Description |
|------|-------------|
| `--adversarial-verify` / `--av` | Enable adversarial spec-compliance review |
| `--av-spec FILE` | Path to the spec file the reviewer checks against. If omitted, the reviewer is told to look for the spec referenced in the original prompt |
| `--av-threshold N` | Minimum score to pass (default: 95) |
| `--av-rounds N` | Max review-fix rounds (default: 3) |
| `--av-model MODEL` | Model for the reviewer (default: same as worker) |
| `--av-prompt FILE` | Custom reviewer prompt template (advanced) |

### New Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDE_AV_THRESHOLD` | `95` | Minimum spec-compliance score to pass |
| `CLAUDE_AV_ROUNDS` | `3` | Max adversarial review-fix rounds |
| `CLAUDE_AV_MODEL` | (none) | Override model for reviewer process |

## Architecture

### Execution Flow

```
┌──────────────────────────────────────────────────────────────────┐
│                       claude-run orchestrator                    │
│                                                                  │
│  1. Worker phase (existing)                                      │
│     ┌──────────────┐                                             │
│     │  Claude #1    │──→ reads spec, writes code                 │
│     │  (worker)     │                                            │
│     └──────────────┘                                             │
│                                                                  │
│  2. Deterministic verify (existing --verify, optional)           │
│     ┌──────────────┐                                             │
│     │  Shell cmd    │──→ make ci, cargo test, etc.               │
│     └──────────────┘                                             │
│                                                                  │
│  3. Adversarial spec-compliance review (new --av)                │
│     ┌──────────────┐       ┌──────────────┐                      │
│     │  Claude #2    │──→──→│  Score        │                     │
│     │  (reviewer)   │  │   │  Parser       │                     │
│     └──────────────┘  │   └──────┬───────┘                      │
│                        │     score │                              │
│                        │          │                               │
│                        │    ≥ 95? ├── YES → done                 │
│                        │          │                               │
│                        │     NO   ▼                               │
│                        │   ┌──────────────┐                      │
│                        │   │  Claude #1    │  (resume)            │
│                        │   │  (worker)     │──→ address gaps     │
│                        │   └──────────────┘                      │
│                        │          │                               │
│                        │    (re-run --verify if present)          │
│                        │          │                               │
│                        └──────────┘  (loop up to N rounds)       │
│                                                                  │
│  4. Exit with final score                                        │
└──────────────────────────────────────────────────────────────────┘
```

### Why a Second Claude Process?

- **Fresh context**: No shared session memory — the reviewer sees the spec and
  the code on disk, not the worker's reasoning or excuses
- **No anchoring**: The worker's internal "I did a good job" narrative doesn't
  influence the reviewer
- **Model diversity**: Use a different (potentially stronger) model for review
- **Simple implementation**: Just another `run_claude()` call through the
  existing `CommandRunner` trait

## Reviewer Prompt

The reviewer prompt is the core of the system. It must produce a structured,
parseable output with a numeric score and itemized findings.

### Default Reviewer Prompt Template

```
You are a strict spec-compliance auditor. Your job is to score how completely
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
fully implemented, output SCORE: 100 with empty sections.
```

### Prompt Construction

```
┌──────────────────────────────────────────┐
│  1. Auditor role framing                 │
│  2. Spec file reference                  │
│  3. Audit process instructions           │
│  4. Scoring rubric                       │
│  5. Structured output format             │
└──────────────────────────────────────────┘
```

When `--av-spec` is provided, `{spec_file}` is the literal path. When omitted,
the prompt says: "The spec is referenced in the original task prompt. The
developer was asked to: {original_prompt}. Find and read the spec file."

## Verdict Parsing

```rust
pub struct ReviewScore {
    pub score: u32,
    pub missing: Vec<String>,
    pub partial: Vec<String>,
    pub incorrect: Vec<String>,
}

pub enum ReviewVerdict {
    Scored(ReviewScore),
    NoVerdict,  // reviewer didn't produce a parseable verdict
}
```

### Parsing Logic

1. Find the last `<verdict>...</verdict>` block in the reviewer's output
2. Extract `SCORE: N` — parse as u32, clamp to 0-100
3. Extract items under `MISSING:`, `PARTIAL:`, `INCORRECT:` sections
4. If no verdict block found → `NoVerdict` (treated as score 0)

`NoVerdict` is treated as a failure, not a pass. This prevents the reviewer
from silently passing by omitting the verdict.

### Score Threshold

```
score >= threshold → PASS (exit the loop)
score <  threshold → FAIL (send worker back to fix)
```

Default threshold: **95** (not 100 — allows minor style deductions without
infinite loops).

## Fix Prompt Construction

When the reviewer scores below threshold, the orchestrator sends the worker
back with a focused fix prompt:

```
A spec-compliance audit scored your implementation {score}/100.
The threshold is {threshold}. You need to address these gaps:

## Missing (not implemented)
{missing items}

## Partial (incomplete implementation)
{partial items}

## Incorrect (wrong behavior)
{incorrect items}

Go through each item and implement it fully. Do not skip any.
Do not add TODO comments — write the actual implementation.
```

Key design choices:
- **Score is included** — creates urgency and a concrete target
- **Items are categorized** — missing vs. partial vs. incorrect need different
  responses from the worker
- **"Do not add TODO comments"** — directly addresses the laziness failure mode
- **Reviewer's reasoning is excluded** — just the actionable list, prevents
  the worker from arguing

## The Adversarial Loop

```rust
pub enum AdversarialOutcome {
    Passed { score: u32, round: u32 },
    ExhaustedRounds { final_score: u32 },
    WorkerFailed { exit_code: i32, round: u32 },
    ReviewerFailed { exit_code: i32, round: u32 },
}

pub async fn run_adversarial_loop<R: CommandRunner>(
    worker: &ClaudeRunner<R>,
    original_prompt: &str,
    verify_cmd: Option<&str>,
    av_config: &AdversarialConfig,
) -> AdversarialOutcome {
    let mut last_score = 0;

    for round in 1..=av_config.max_rounds {
        output::av_round(round, av_config.max_rounds);

        // 1. Launch reviewer (fresh session, captures stdout)
        let review_prompt = build_review_prompt(
            original_prompt,
            &av_config.spec_file,
            round,
        );
        let reviewer_args = build_reviewer_args(
            &review_prompt,
            av_config,
            &worker.session_name,
            round,
        );
        let review_result = worker.cmd
            .run_claude_capturing(&reviewer_args)
            .await?;

        // 2. Parse verdict
        let verdict = parse_verdict(&review_result.stdout);
        let score = match &verdict {
            ReviewVerdict::Scored(s) => {
                output::av_score(s.score, av_config.threshold);
                last_score = s.score;
                s
            }
            ReviewVerdict::NoVerdict => {
                output::av_no_verdict();
                last_score = 0;
                // Synthesize a zero-score result
                &ReviewScore {
                    score: 0,
                    missing: vec!["Reviewer did not produce a verdict".into()],
                    partial: vec![],
                    incorrect: vec![],
                }
            }
        };

        // 3. Check threshold
        if score.score >= av_config.threshold {
            output::av_passed(score.score);
            return AdversarialOutcome::Passed {
                score: score.score,
                round,
            };
        }

        // 4. Last round? Don't fix, just report.
        if round == av_config.max_rounds {
            break;
        }

        // 5. Send worker back to fix
        let fix_prompt = build_fix_prompt(score, av_config.threshold);
        output::av_fixing(score.score, av_config.threshold);
        worker.run_with_retry(&fix_prompt, true).await?;

        // 6. Re-run deterministic verify if present
        if let Some(cmd) = verify_cmd {
            match verify::run_verify_loop(worker, cmd).await {
                VerifyOutcome::Passed { .. } => {}
                VerifyOutcome::ExhaustedRounds => {
                    return AdversarialOutcome::WorkerFailed {
                        exit_code: 1,
                        round,
                    };
                }
                VerifyOutcome::ClaudeFailed { exit_code, .. } => {
                    return AdversarialOutcome::WorkerFailed {
                        exit_code,
                        round,
                    };
                }
            }
        }
    }

    output::av_exhausted(last_score, av_config.threshold, av_config.max_rounds);
    AdversarialOutcome::ExhaustedRounds {
        final_score: last_score,
    }
}
```

## Configuration

### `AdversarialConfig` Struct

```rust
pub struct AdversarialConfig {
    pub max_rounds: u32,                 // --av-rounds / CLAUDE_AV_ROUNDS (default: 3)
    pub threshold: u32,                  // --av-threshold / CLAUDE_AV_THRESHOLD (default: 95)
    pub spec_file: Option<String>,       // --av-spec
    pub reviewer_model: Option<String>,  // --av-model / CLAUDE_AV_MODEL
    pub custom_prompt: Option<String>,   // --av-prompt (file path or inline)
}
```

Separate from `Config` — only constructed when `--av` is present.

## Execution Order

When both `--verify` and `--av` are present:

```
Worker → Verify Loop → Adversarial Loop
                              │
                        ┌─────┴──────┐
                        │  reviewer   │
                        │  scores     │
                        └─────┬──────┘
                         < threshold?
                              │
                        worker fixes
                              │
                        re-run verify loop
                              │
                        back to reviewer
```

Deterministic verify runs first because:
1. It's fast and free (no API calls)
2. No point scoring code that doesn't compile
3. After worker fixes reviewer feedback, re-run verify to catch regressions

## Reviewer Session Naming

```
worker:   impl-logi-feat
reviewer: impl-logi-feat-av-1  (round 1)
reviewer: impl-logi-feat-av-2  (round 2)
```

Each review round gets a fresh session (no `--continue`). The reviewer must
evaluate the current state from scratch, not build on its previous review.

## Runner Change: Capturing Stdout

The current `run_claude()` inherits stdout (streams to terminal). The reviewer
needs stdout **captured** for verdict parsing.

Add a new method to `CommandRunner`:

```rust
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run_claude(&self, args: &[String]) -> io::Result<RunResult>;
    async fn run_claude_capturing(&self, args: &[String]) -> io::Result<RunResult>;
    async fn run_shell(&self, cmd: &str) -> io::Result<RunResult>;
}
```

`run_claude_capturing` pipes stdout and captures it in `RunResult.stdout`.
Stderr is still streamed to terminal and captured in `RunResult.stderr`.
The reviewer's output is also printed to the terminal (tee'd) so the user
can follow along.

## Changes to Existing Modules

| Module | Change |
|--------|--------|
| `cli.rs` | Add `--av`, `--av-spec`, `--av-threshold`, `--av-rounds`, `--av-model`, `--av-prompt` flags |
| `lib.rs` | Wire adversarial loop after verify loop |
| `output.rs` | Add `av_round()`, `av_score()`, `av_passed()`, `av_fixing()`, `av_exhausted()`, `av_no_verdict()` |
| `runner.rs` | Add `run_claude_capturing()` to trait + `TokioCommandRunner` impl |

New module: `src/adversarial.rs` — config, verdict parsing, prompt building,
loop orchestration.

## Example Usage

```bash
# Basic: worker implements spec, reviewer audits
claude-run --av --av-spec spec.md "implement the spec in spec.md"

# With tests: tests must pass, then reviewer audits
claude-run --av --av-spec spec.md --verify "make ci" "implement the spec"

# Higher bar
claude-run --av --av-threshold 100 --av-spec spec.md "implement the spec"

# More retries, stronger reviewer
claude-run --av --av-rounds 5 --av-model opus "implement the spec"

# Lower bar for drafts
claude-run --av --av-threshold 80 --av-spec spec.md "implement the spec"
```

## Terminal Output

```
╭───────────────────────────────────────╮
│  claude-run  session: impl-spec-feat  │
│  verify: make ci                      │
│  adversarial: spec.md (threshold: 95) │
│  started: 2026-04-01 10:30:00         │
╰───────────────────────────────────────╯

[Claude working...]

✓ Verification passed (round 1)

⚔ Adversarial review (round 1/3)
  Reviewer scoring against spec.md...
  Score: 72/100 (threshold: 95)
  Missing: 3 items | Partial: 2 items | Incorrect: 1 item

⚔ Sending worker back to address 6 issues...
[Claude fixing...]

✓ Verification passed (round 1)

⚔ Adversarial review (round 2/3)
  Reviewer scoring against spec.md...
  Score: 91/100 (threshold: 95)
  Missing: 0 items | Partial: 1 item | Incorrect: 1 item

⚔ Sending worker back to address 2 issues...
[Claude fixing...]

✓ Verification passed (round 1)

⚔ Adversarial review (round 3/3)
  Reviewer scoring against spec.md...
  Score: 98/100 (threshold: 95)

✓ Adversarial review passed (98/100, round 3)

✓ Done: impl-spec-feat
```

## Edge Cases & Failure Modes

### Score inflation
The reviewer might be too generous. Mitigations:
- Prompt explicitly says "your job is to find gaps, not be encouraging"
- Default threshold is high (95) so even generous scores need near-completeness
- `--av-model` lets you use a more critical model

### Score deflation / hallucinated gaps
The reviewer might flag things that are actually implemented. Mitigations:
- Prompt requires citing file paths and line numbers (grounds claims)
- After the worker "fixes" (or does nothing because it's already done), the
  next review round should score higher
- Round limit prevents infinite loops

### Worker adds TODOs instead of implementations
The fix prompt explicitly says "Do not add TODO comments — write the actual
implementation." The reviewer in the next round will also catch this since
TODOs get scored as partial implementations (-5 to -10).

### Reviewer rate-limited
The reviewer launches through `run_with_retry`, getting the same backoff
and daily cap handling as the worker.

### Spec file doesn't exist
If `--av-spec` points to a nonexistent file, the reviewer will report it
can't find the spec. The orchestrator could also validate the path upfront
and fail fast with a clear error.

### Score not parseable
Treated as `NoVerdict` → score 0 → worker gets sent back with generic
"implement the spec more completely" guidance.

## Implementation Plan

1. `src/adversarial.rs` — `AdversarialConfig`, `ReviewScore`, `ReviewVerdict`,
   `parse_verdict()`, `build_review_prompt()`, `build_fix_prompt()`,
   `run_adversarial_loop()`
2. `src/runner.rs` — add `run_claude_capturing()` to `CommandRunner` trait
3. `src/cli.rs` — add `--av*` flags, wire into `Cli` struct
4. `src/output.rs` — add adversarial output functions
5. `src/lib.rs` — integrate adversarial loop after verify loop
6. Tests: verdict parsing, prompt building, loop with mock runner
