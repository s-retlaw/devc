//! End-to-end tests that require a real container runtime (Docker or Podman).
//! All tests are `#[ignore]` so they only run when explicitly opted in:
//!
//!   cargo test -p devc-cli -- --ignored

#![allow(deprecated)] // assert_cmd::Command::cargo_bin is deprecated but works fine

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
#[ignore]
fn test_full_lifecycle() {
    // Full lifecycle: init in a temp dir with a devcontainer.json
    let tmp = tempfile::tempdir().unwrap();
    let devcontainer_dir = tmp.path().join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).unwrap();
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        r#"{"image": "ubuntu:22.04"}"#,
    )
    .unwrap();

    Command::cargo_bin("devc")
        .unwrap()
        .args(["init"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized container"));
}

#[test]
#[ignore]
fn test_build_and_up() {
    // Build and bring up a container from a workspace with devcontainer.json.
    // Uses ubuntu which has /bin/bash (alpine only has /bin/sh).
    let tmp = tempfile::tempdir().unwrap();
    let devcontainer_dir = tmp.path().join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).unwrap();
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        r#"{"image": "ubuntu:22.04"}"#,
    )
    .unwrap();

    // init + up
    Command::cargo_bin("devc")
        .unwrap()
        .args(["up"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // list should show the container as running
    Command::cargo_bin("devc")
        .unwrap()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("running"));
}

#[test]
#[ignore]
fn test_exec_in_running_container() {
    // Placeholder — depends on a running container from test_build_and_up.
    // nextest runs tests in isolation so we can't depend on ordering.
    eprintln!("Skipped: requires running container from prior test");
}

#[test]
#[ignore]
fn test_stop_and_remove() {
    // Placeholder — depends on a running container from test_build_and_up.
    eprintln!("Skipped: requires running container from prior test");
}
