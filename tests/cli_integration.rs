use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_flag_shows_usage() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-run"))
        .stdout(predicate::str::contains("--verify"))
        .stdout(predicate::str::contains("--resume"))
        .stdout(predicate::str::contains("--name"));
}

#[test]
fn no_args_exits_with_error() {
    Command::cargo_bin("claude-run").unwrap().assert().failure();
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
fn pipeline_with_verify_is_error() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .args(["--pipeline", "test.yaml", "--verify", "make test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be combined"));
}

#[test]
fn pipeline_with_prompt_is_error() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .args(["--pipeline", "test.yaml", "some prompt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be combined"));
}

#[test]
fn pipeline_with_av_is_error() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .args(["--pipeline", "test.yaml", "--av"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be combined"));
}

#[test]
fn help_mentions_pipeline() {
    Command::cargo_bin("claude-run")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--pipeline"));
}
