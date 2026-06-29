//! CLI integration tests.
//!
//! Drives the real binary with `assert_cmd`. The `--check` / update flows are
//! wired in a later cluster; these baseline tests assert the binary builds and
//! exposes `--help` / `--version`, catching "binary does not even start".

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn shows_help() {
    Command::cargo_bin("pwsh-autoupdate")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("pwsh-autoupdate"));
}

#[test]
fn shows_version() {
    Command::cargo_bin("pwsh-autoupdate")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}
