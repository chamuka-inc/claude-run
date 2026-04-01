# claude-run

Non-interactive CLI that orchestrates [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions. Four composable subcommands — each does one thing, all work together.

## Install

```bash
cargo install --path crates/cli
```

Requires the `claude` CLI to be installed and on your PATH.

## Subcommands

### `retry` — Rate-limit retry wrapper

Wraps a Claude invocation with exponential backoff, daily-cap polling, and auto-resume.

```bash
claude-run retry "implement the login feature"
claude-run retry --name my-feat --model opus "implement the feature"
claude-run retry --resume my-feat
```

### `verify` — Generic verify-fix loop

Run a worker command, then a check command. If the check fails, feed the error output back to the worker. Not Claude-specific — works with any commands.

```bash
claude-run verify \
  --worker 'claude-run retry --name feat "implement login"' \
  --check 'make test'
```

### `review` — Adversarial spec-compliance scorer

Launch an independent Claude instance that reads a spec and scores the current implementation 0-100. Outputs a structured verdict to stdout.

```bash
claude-run review --spec spec.md --threshold 95
claude-run review --spec spec.md --model opus --threshold 90
```

Exit code 0 if score >= threshold, 1 if below.

### `pipeline` — YAML multi-stage orchestrator

Execute complex multi-stage workflows defined in YAML.

```bash
claude-run pipeline pipeline.yaml
```

## Composition

The power is in how subcommands compose:

```bash
# Simple: implement + test
claude-run verify \
  --worker 'claude-run retry --name feat "implement login"' \
  --check 'make test'

# With adversarial review (chain checks with &&)
claude-run verify \
  --worker 'claude-run retry --name feat "implement the spec"' \
  --check 'make ci && claude-run review --spec spec.md --threshold 95'
```

Or define it all in YAML:

```yaml
stages:
  - name: implement
    type: claude
    prompt: "Implement the spec in spec.md."

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

Cargo workspace with one binary and a shared library:

```
crates/
├── lib/          Shared engine: pipeline runner, stage execution,
│                 rate-limit retry, verdict parsing, YAML loader
├── retry/        claude-run retry subcommand
├── verify/       claude-run verify subcommand
├── review/       claude-run review subcommand
├── pipeline/     claude-run pipeline subcommand
└── cli/          Main binary: subcommand dispatcher
```

## Development

```bash
make check    # fmt + clippy + test (full workspace)
make ci       # same as check
make deploy   # build release + install
```
