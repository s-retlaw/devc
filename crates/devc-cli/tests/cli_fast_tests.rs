//! Fast CLI tests using assert_cmd.
//! These test the binary directly without needing a container runtime.

#![allow(deprecated)] // assert_cmd::Command::cargo_bin is deprecated but works fine

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_help_flag() {
    Command::cargo_bin("devc")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Dev Container Manager"));
}

#[test]
fn test_version_flag() {
    Command::cargo_bin("devc")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}

#[test]
fn test_subcommand_help() {
    for subcmd in &["build", "shell", "exec", "start", "stop", "list", "init"] {
        Command::cargo_bin("devc")
            .unwrap()
            .args([subcmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::is_empty().not());
    }
}

#[test]
fn test_unknown_subcommand_fails() {
    Command::cargo_bin("devc")
        .unwrap()
        .arg("nonexistent-subcommand")
        .assert()
        .failure();
}

#[test]
fn test_init_no_devcontainer_fails() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("devc")
        .unwrap()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("devcontainer.json"));
}

#[test]
fn test_config_shows_output() {
    Command::cargo_bin("devc")
        .unwrap()
        .arg("config")
        .assert()
        .success();
}

#[test]
fn test_list_succeeds() {
    // List should succeed even with no containers (prints "No containers found")
    Command::cargo_bin("devc")
        .unwrap()
        .arg("list")
        .assert()
        .success();
}

#[test]
fn test_up_help() {
    Command::cargo_bin("devc")
        .unwrap()
        .args(["up", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Build, create, and start"));
}
