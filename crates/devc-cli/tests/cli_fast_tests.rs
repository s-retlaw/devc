//! Fast CLI tests using assert_cmd.
//! These test the binary directly without needing a container runtime.

#![allow(deprecated)] // assert_cmd::Command::cargo_bin is deprecated but works fine

use assert_cmd::Command;
use predicates::prelude::*;

fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

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
    if !docker_available() {
        eprintln!("Skipping: docker not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("devc")
        .unwrap()
        .args(["init"])
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
    if !docker_available() {
        eprintln!("Skipping: docker not available");
        return;
    }
    // List should succeed even with no containers (prints "No containers found")
    Command::cargo_bin("devc")
        .unwrap()
        .args(["list"])
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
