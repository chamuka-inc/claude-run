# Adversarial Verification Design

## Problem

The current `--verify` flag runs a **shell command** (e.g. `make test`) to check
Claude's work. This catches regressions that have deterministic test coverage,
but misses an entire class of issues:

- Subtle logic errors that pass tests but violate intent
- Security vulnerabilities not covered by existing tests
- Tests that were written by the same Claude session (marking its own homework)
- Prompt-adherence issues (code works but doesn't match what was asked)
- Code quality issues, unnecessary complexity, dead code
- Edge cases the original session didn't consider

**Adversarial verification** solves this by launching a second, independent
Claude process whose sole job is to critically review the first process's work
and find problems.

## Design Principles

1. **Composition over complexity** — reuse `claude-run` itself as the reviewer
2. **Adversarial by default** — the reviewer's system prompt is skeptical, not helpful
3. **Structured verdicts** — the reviewer outputs a machine-parseable pass/fail
4. **Independent context** — the reviewer gets a fresh session (no shared memory with the worker)
5. **Combine, don't replace** — adversarial review works alongside `--verify`, not instead of it

## CLI Interface

```
claude-run --adversarial-verify [OPTIONS] "prompt"
claude-run --adversarial-verify --verify "make ci" "prompt"
```

### New Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--adversarial-verify` | `--av` | Enable adversarial verification after worker completes |
| `--av-prompt FILE\|STR` | | Custom reviewer prompt (default: built-in) |
| `--av-rounds N` | | Max review-fix rounds (default: 3) |
| `--av-model MODEL` | | Model for the reviewer (default: same as worker) |

### New Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDE_AV_ROUNDS` | `3` | Max adversarial review-fix rounds |
| `CLAUDE_AV_MODEL` | (none) | Override model for reviewer process |

## Architecture

### Execution Flow

```
┌─────────────────────────────────────────────────────────────┐
│                      claude-run orchestrator                │
│                                                             │
│  1. Worker phase (existing)                                 │
│     ┌──────────────┐                                        │
│     │  Claude #1    │──→ writes code, makes changes         │
│     │  (worker)     │                                       │
│     └──────────────┘                                        │
│                                                             │
│  2. Deterministic verify (existing --verify)                │
│     ┌──────────────┐                                        │
│     │  Shell cmd    │──→ make ci, cargo test, etc.          │
│     └──────────────┘                                        │
│                                                             │
│  3. Adversarial verify (new --adversarial-verify)           │
│     ┌──────────────┐       ┌──────────────┐                 │
│     │  Claude #2    │──→──→│  Verdict      │                │
│     │  (reviewer)   │  │   │  Parser       │                │
│     └──────────────┘  │   └──────┬───────┘                 │
│                        │          │                          │
│           review       │     PASS │ FAIL                    │
│           prompt       │          │                          │
│                        │          ▼                          │
│                        │   ┌──────────────┐                 │
│                        │   │  Claude #1    │  (resume)       │
│                        │   │  (worker)     │──→ fix issues   │
│                        │   └──────────────┘                 │
│                        │          │                          │
│                        └──────────┘  (loop up to N rounds)  │
│                                                             │
│  4. Notify                                                  │
└─────────────────────────────────────────────────────────────┘
```

### Key Design Decision: Reviewer as a Separate Claude Process

The reviewer runs as a **separate `claude` invocation** (not `claude-run`). This
is critical because:

- **Fresh context**: No shared session memory with the worker — the reviewer
  sees only the code on disk and the git diff, not the worker's reasoning
- **No tool leakage**: The reviewer doesn't inherit the worker's session state
- **Independent judgment**: Can use a different model (e.g. worker=sonnet,
  reviewer=opus) for diversity of thought
- **Simple implementation**: Just another `run_claude()` call through the
  existing `CommandRunner` trait

### Reviewer Prompt Construction

The orchestrator builds the reviewer's prompt from three pieces:

```
┌──────────────────────────────────────────┐
│  1. System framing (adversarial role)    │
│  2. Original task context                │
│  3. Review instructions + verdict format │
└──────────────────────────────────────────┘
```

#### Default Reviewer Prompt Template

```
You are a critical code reviewer performing adversarial verification.
Your job is to find problems, not to be helpful or encouraging.

## Original Task
The developer was asked to: {original_prompt}

## Your Review Process
1. Read the git diff to understand what changed: run `git diff HEAD~1` (or
   appropriate range)
2. Read the affected files in full context
3. Try to find:
   - Logic errors or off-by-one bugs
   - Security vulnerabilities (injection, auth bypass, etc.)
   - Missing error handling at system boundaries
   - Cases where the code doesn't match the stated task
   - Tests that don't actually test what they claim
   - Regressions in existing functionality

## Verdict
After your review, you MUST output a verdict block as the very last thing:

If everything looks correct:
<verdict>PASS</verdict>

If you found real issues that need fixing:
<verdict>FAIL
- issue 1 description
- issue 2 description
</verdict>

Only flag genuine issues. Do not flag style preferences, naming opinions,
or hypothetical concerns. Be specific — cite file paths and line numbers.
```

### Verdict Parsing

The orchestrator captures the reviewer's stdout and parses the verdict:

```rust
enum ReviewVerdict {
    Pass,
    Fail { issues: Vec<String> },
    NoVerdict, // reviewer didn't output a verdict block
}

fn parse_verdict(output: &str) -> ReviewVerdict {
    // Find last <verdict>...</verdict> block in output
    // Parse PASS vs FAIL + issue list
}
```

`NoVerdict` is treated as `Fail` with a generic "reviewer did not produce a
structured verdict" message — this prevents the reviewer from silently passing
by omission.

### Fix Prompt Construction

When the reviewer finds issues, the orchestrator sends the worker back in with:

```
An independent code reviewer found the following issues with your work:

{issues}

Fix these issues. Do not argue with the reviewer — address each point
with a concrete code change.
```

This deliberately doesn't include the reviewer's full reasoning — just the
actionable issue list. This prevents the worker from "debating" the reviewer
and forces it to address the concrete feedback.

### The Adversarial-Verify Loop

```rust
pub async fn run_adversarial_loop<R: CommandRunner>(
    worker: &ClaudeRunner<R>,
    original_prompt: &str,
    av_config: &AdversarialConfig,
) -> AdversarialOutcome {
    for round in 1..=av_config.max_rounds {
        // 1. Launch reviewer
        let review_prompt = build_review_prompt(original_prompt, round);
        let review_result = worker.cmd.run_claude(&build_reviewer_args(
            &review_prompt,
            &av_config,
            &worker.session_name,
            round,
        )).await;

        // 2. Parse verdict
        let verdict = parse_verdict(&review_result.stdout);

        match verdict {
            ReviewVerdict::Pass => return AdversarialOutcome::Passed { round },
            ReviewVerdict::Fail { issues } | ReviewVerdict::NoVerdict => {
                // 3. Send worker back to fix
                let fix_prompt = build_fix_prompt(&issues);
                worker.run_with_retry(&fix_prompt, true).await?;

                // 4. Optionally re-run deterministic verify
                //    (if --verify was also provided)
            }
        }
    }
    AdversarialOutcome::ExhaustedRounds
}
```

### Reviewer Session Naming

The reviewer gets its own session name to avoid colliding with the worker:

```
worker:   impl-logi-feat
reviewer: impl-logi-feat-av-1  (round 1)
reviewer: impl-logi-feat-av-2  (round 2)
```

## Configuration

### `AdversarialConfig` Struct

```rust
pub struct AdversarialConfig {
    pub max_rounds: u32,        // CLAUDE_AV_ROUNDS (default: 3)
    pub reviewer_model: Option<String>,  // CLAUDE_AV_MODEL
    pub custom_prompt: Option<String>,   // --av-prompt
}
```

### Integration with Existing Config

`AdversarialConfig` is a separate struct (not bolted onto `Config`) because:
- It's only relevant when `--adversarial-verify` is active
- It has its own distinct env vars
- Keeps the existing `Config` clean for the common case

## Execution Order

When both `--verify` and `--adversarial-verify` are present:

```
Worker → Deterministic Verify Loop → Adversarial Verify Loop
                                            │
                                     (on fix, re-run deterministic verify too)
```

The deterministic verify runs **first** because:
1. It's fast and cheap (no API calls)
2. No point having the reviewer look at code that doesn't even compile/pass tests
3. After the reviewer's fixes, we re-run deterministic verify to ensure the
   fix didn't break anything

### Full Combined Loop

```
1. Worker runs
2. --verify loop runs (up to verify_max rounds)
3. If --verify passes:
   a. Reviewer runs (round 1)
   b. If FAIL: worker fixes, then re-run --verify loop, then back to (a)
   c. If PASS: done
   d. Repeat up to av_rounds
4. Exit
```

## New Module: `src/adversarial.rs`

```rust
// New types
pub struct AdversarialConfig { ... }
pub enum ReviewVerdict { Pass, Fail { issues: Vec<String> }, NoVerdict }
pub enum AdversarialOutcome {
    Passed { round: u32 },
    ExhaustedRounds,
    WorkerFailed { exit_code: i32, round: u32 },
    ReviewerFailed { exit_code: i32, round: u32 },
}

// New functions
pub fn build_review_prompt(original_prompt: &str, round: u32) -> String
pub fn build_fix_prompt(issues: &[String]) -> String
pub fn parse_verdict(output: &str) -> ReviewVerdict
pub async fn run_adversarial_loop<R: CommandRunner>(...) -> AdversarialOutcome
```

## Changes to Existing Modules

| Module | Change |
|--------|--------|
| `cli.rs` | Add `--adversarial-verify`, `--av-prompt`, `--av-rounds`, `--av-model` |
| `config.rs` | Add `AdversarialConfig::from_env()` |
| `lib.rs` | Wire adversarial loop after verify loop |
| `output.rs` | Add `adversarial_round()`, `adversarial_passed()`, etc. |
| `runner.rs` | Add `run_claude_capturing()` variant that captures stdout |

### Runner Change: Capturing Stdout

The current `run_claude()` streams stdout to terminal (inherits). The reviewer
needs stdout **captured** so we can parse the verdict. Two options:

**Option A: New method on `CommandRunner`**
```rust
async fn run_claude_capturing(&self, args: &[String]) -> io::Result<RunResult>;
```
This pipes stdout instead of inheriting it. We still stream stderr. The captured
stdout is returned in `RunResult.stdout`.

**Option B: Use `--output-format json` flag**
Claude supports `--output-format json` which writes structured output. We could
parse the JSON for the final response text.

**Recommendation: Option A.** It's simpler, more reliable, and doesn't depend
on Claude's JSON output format staying stable.

## Example Usage

### Basic adversarial review
```bash
claude-run --adversarial-verify "implement user authentication"
```

### With deterministic verify too
```bash
claude-run --adversarial-verify --verify "make ci" "implement user authentication"
```

### Reviewer uses a different (stronger) model
```bash
claude-run --av-model opus --adversarial-verify "implement user authentication"
```

### Custom reviewer prompt from file
```bash
claude-run --av-prompt security-review.txt --adversarial-verify "implement user authentication"
```

### Limit review rounds
```bash
claude-run --av-rounds 1 --adversarial-verify "implement user authentication"
```

## Edge Cases & Failure Modes

### Reviewer hallucinates issues
The reviewer might flag non-issues. Mitigations:
- The prompt instructs "only flag genuine issues"
- Round limit prevents infinite loops
- The worker can "fix" by explaining in a comment why it's not an issue
  (the reviewer in the next round may then PASS)

### Worker undoes previous fix
The worker might fix issue A but reintroduce issue B. Mitigations:
- Each review round sees the full current state
- Combining with `--verify` catches regressions

### Reviewer never passes
Max rounds (default 3) prevents infinite loops. The orchestrator reports
`ExhaustedRounds` and exits non-zero, same as the deterministic verify.

### Reviewer rate-limited
The reviewer runs through the same `run_with_retry` mechanism, so it gets
the same exponential backoff and daily cap handling as the worker.

### Both processes write to the same files
This is safe because they run **sequentially**, never concurrently. The worker
writes, then the reviewer reads (and doesn't write — it's `--permission-mode
bypassPermissions` but its prompt only asks it to review, not modify).

## Future Extensions (Out of Scope)

- **Multi-reviewer panel**: Run N reviewers in parallel, majority vote
- **Specialized reviewers**: Security reviewer, performance reviewer, etc.
- **Review memory**: Feed previous round's review into the next round for continuity
- **Confidence scoring**: Reviewer rates confidence in each issue
- **Git worktree isolation**: Run reviewer in a separate worktree for true isolation

## Implementation Plan

1. Add `adversarial.rs` with config, verdict parsing, prompt building
2. Add `run_claude_capturing()` to `CommandRunner` trait
3. Wire up CLI flags in `cli.rs`
4. Add output formatting in `output.rs`
5. Integrate into `lib.rs` execution flow
6. Tests for verdict parsing, prompt building, loop behavior
7. Integration test with mock runner
