# claude-run

Non-interactive CLI that orchestrates [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions with automatic rate-limit retry, verification loops, adversarial spec-compliance review, and multi-stage YAML pipelines.

## Install

```bash
cargo install --path .
```

Requires the `claude` CLI to be installed and on your PATH.

## Quick Start

```bash
# Run Claude with automatic rate-limit retry
claude-run "implement the login feature"

# Run + verify: loop until tests pass
claude-run --verify "make ci" "implement the login feature"

# Adversarial review: a second Claude scores spec compliance
claude-run --av --av-spec spec.md --verify "make ci" "implement the spec"

# Multi-stage pipeline from YAML
claude-run --pipeline pipeline.yaml
```

## How It Works

Every use case follows the same pattern: **run work, check work, loop on failure**.

```
claude-run "implement feature" --verify "make test"

  ┌─────────────────┐
  │  Claude (worker) │──→ implements feature
  └────────┬────────┘
           │
  ┌────────▼────────┐
  │  make test       │──→ runs tests
  └────────┬────────┘
           │
      pass? → done
      fail? → send Claude back with the error output
           │
      (repeat up to 5 rounds)
```

### Rate-Limit Recovery

Every Claude invocation gets automatic retry with exponential backoff. If fast retries are exhausted, switches to daily-cap polling mode — probes periodically until the cap resets, then resumes the session.

### Verify-Fix Loop

After Claude finishes, run a shell command to verify. If it fails, the error output is sent back to Claude with `--continue` to fix. Repeats up to `CLAUDE_VERIFY_MAX` rounds.

### Adversarial Verification

A second, independent Claude instance reads the spec and scores the implementation 0-100. If the score is below threshold (default: 95), the worker gets itemized feedback (missing, partial, incorrect) and tries again.

```bash
claude-run --av --av-spec spec.md --verify "make ci" "implement the spec"
```

This chains: tests must pass first (fast, free), then the reviewer scores against the spec. The reviewer gets a fresh session each round — no shared context with the worker.

### Multi-Stage YAML Pipelines

For complex workflows, define stages in YAML:

```yaml
stages:
  - name: implement
    type: claude
    prompt: "Implement the spec in spec.md. Production code only."

  - name: write-tests
    type: claude
    prompt: "Read the spec and write comprehensive tests."
    session_suffix: "-tests"
    model: sonnet

  - name: checks
    type: parallel
    stages:
      - name: lint
        type: shell
        command: "make lint"
      - name: typecheck
        type: shell
        command: "make typecheck"

  - name: verify
    type: verify-loop
    worker: implement
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
          prompt: "Score the implementation 0-100."
```

```bash
claude-run --pipeline pipeline.yaml
```

#### Stage Types

| Type | Description |
|------|-------------|
| `claude` | Run a Claude Code instance with a prompt |
| `shell` | Run a shell command |
| `verify-loop` | Run a worker, verify, loop on failure |
| `parallel` | Run inner stages concurrently |

#### Verifier Types

| Type | Description |
|------|-------------|
| `shell` | Pass if exit code 0 |
| `claude` | Run a reviewer, parse verdict (`score`, `passfail`, `exitcode`) |
| `chain` | Run multiple verifiers in sequence (all must pass) |

## CLI Reference

```
Usage: claude-run [OPTIONS] "prompt"
       claude-run --resume [session-name]
       claude-run --pipeline pipeline.yaml

Options:
  --name NAME        Session name (default: auto-generated from prompt)
  --resume [NAME]    Resume last session, or a named session
  --verify CMD       Verify after Claude finishes; loop on failure
  --pipeline FILE    Load multi-stage pipeline from YAML
  --help, -h         Show help
  --version, -v      Show version

Adversarial Verification:
  --av               Enable adversarial spec-compliance review
  --av-spec FILE     Spec file for the reviewer
  --av-threshold N   Minimum score to pass (default: 95)
  --av-rounds N      Max review-fix rounds (default: 3)
  --av-model MODEL   Model for reviewer (default: same as worker)

All other flags pass through to claude (e.g. --max-turns 50, --model opus).
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDE_MAX_RETRIES` | `10` | Max rate-limit retries before daily-cap mode |
| `CLAUDE_RETRY_DELAY` | `60` | Initial backoff in seconds |
| `CLAUDE_RETRY_CAP` | `300` | Max backoff in seconds |
| `CLAUDE_NOTIFY` | `1` | macOS notification on completion |
| `CLAUDE_VERIFY_MAX` | `5` | Max verify-fix cycles |
| `CLAUDE_DAILY_CAP_POLL` | `300` | Poll interval for daily cap reset |
| `CLAUDE_DAILY_CAP_TIMEOUT` | `28800` | Max wait for cap reset (8 hours) |
| `CLAUDE_AV_THRESHOLD` | `95` | Minimum spec-compliance score |
| `CLAUDE_AV_ROUNDS` | `3` | Max adversarial review rounds |
| `CLAUDE_AV_MODEL` | (none) | Override model for reviewer |

## Architecture

```
src/
├── main.rs           Entry point
├── lib.rs            CLI → Pipeline → PipelineRunner
├── cli.rs            Argument parsing (pass-through for unknown flags)
├── config.rs         Environment variable configuration
├── stage.rs          Stage (Claude | Shell) — unit of work
├── verifier.rs       Verifier (Shell | Claude | Chain) — checks work
├── pipeline.rs       Pipeline, PipelineStep, PipelineRunner
├── verdict.rs        <verdict>SCORE: N</verdict> parsing
├── prompts.rs        Reviewer and fix prompt templates
├── yaml_pipeline.rs  YAML → Pipeline deserialization
├── runner.rs         CommandRunner trait (subprocess abstraction)
├── rate_limit.rs     Rate-limit detection and exponential backoff
├── output.rs         Terminal output formatting
├── notify.rs         macOS notifications
└── slugify.rs        Prompt → session name
```

## Development

```bash
make check    # fmt + clippy + test
make ci       # same as check
make deploy   # build release + install
```
