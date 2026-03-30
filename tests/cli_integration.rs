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
