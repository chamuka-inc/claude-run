use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn no_args_shows_help() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn help_flag() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-run"))
        .stdout(predicate::str::contains("retry"))
        .stdout(predicate::str::contains("verify"))
        .stdout(predicate::str::contains("review"))
        .stdout(predicate::str::contains("pipeline"));
}

#[test]
fn version_flag() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-run"));
}

#[test]
fn unknown_command() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("foobar")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown command"));
}

#[test]
fn retry_help() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .args(["retry", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("retry"));
}

#[test]
fn retry_no_prompt() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("retry")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No prompt"));
}

#[test]
fn verify_help() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .args(["verify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("verify"));
}

#[test]
fn verify_missing_flags() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("verify")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--worker"));
}

#[test]
fn review_help() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .args(["review", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("review"));
}

#[test]
fn review_missing_spec() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("review")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--spec"));
}

#[test]
fn pipeline_help() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .args(["pipeline", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pipeline"));
}

#[test]
fn pipeline_missing_file() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("pipeline")
        .assert()
        .failure()
        .stderr(predicate::str::contains("YAML file"));
}
