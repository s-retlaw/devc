//! End-to-end tests that require a real container runtime (Docker or Podman).
//! All tests are `#[ignore]` so they only run when explicitly opted in:
//!
//!   cargo test -p devc-cli -- --ignored

#![allow(deprecated)] // assert_cmd::Command::cargo_bin is deprecated but works fine

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn create_test_devc_env() -> TempDir {
    let root = tempfile::tempdir().expect("failed to create test env dir");
    let state = root.path().join("state");
    let config = root.path().join("config");
    let cache = root.path().join("cache");
    std::fs::create_dir_all(&state).expect("create DEVC_STATE_DIR");
    std::fs::create_dir_all(&config).expect("create DEVC_CONFIG_DIR");
    std::fs::create_dir_all(&cache).expect("create DEVC_CACHE_DIR");
    root
}

fn apply_devc_env(cmd: &mut Command, root: &TempDir) {
    cmd.env("DEVC_STATE_DIR", root.path().join("state"))
        .env("DEVC_CONFIG_DIR", root.path().join("config"))
        .env("DEVC_CACHE_DIR", root.path().join("cache"));
}

#[test]
#[ignore]
fn test_full_lifecycle() {
    let xdg = create_test_devc_env();
    // Full lifecycle: init in a temp dir with a devcontainer.json
    let tmp = tempfile::tempdir().unwrap();
    let devcontainer_dir = tmp.path().join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).unwrap();
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        r#"{"image": "ubuntu:22.04"}"#,
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut cmd, &xdg);
    cmd.args(["init"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized container"));
}

#[test]
#[ignore]
fn test_build_and_up() {
    let xdg = create_test_devc_env();
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
    let mut up = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut up, &xdg);
    up.args(["up"]).current_dir(tmp.path()).assert().success();

    // list should show the container as running
    let mut list = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut list, &xdg);
    list.args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("running"));
}

#[test]
#[ignore]
fn test_exec_in_running_container() {
    let xdg = create_test_devc_env();
    let tmp = tempfile::tempdir().unwrap();
    let devcontainer_dir = tmp.path().join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).unwrap();
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        r#"{"image": "ubuntu:22.04", "remoteUser": "ubuntu"}"#,
    )
    .unwrap();

    // Bring up the container
    let mut up = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut up, &xdg);
    up.args(["up"]).current_dir(tmp.path()).assert().success();

    // Get the container name from list output
    let mut list = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut list, &xdg);
    let list_out = list.args(["list"]).output().unwrap();
    let list_str = String::from_utf8_lossy(&list_out.stdout);
    // Extract container name from the list output (first running container)
    let container_name = list_str
        .lines()
        .find(|l| l.contains("running"))
        .and_then(|l| l.split_whitespace().next())
        .expect("should have a running container");

    // exec without --root should run as the remoteUser (ubuntu)
    let mut exec_default = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut exec_default, &xdg);
    exec_default
        .args(["exec", container_name, "whoami"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ubuntu"));

    // exec with --root should run as root
    let mut exec_root = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut exec_root, &xdg);
    exec_root
        .args(["exec", container_name, "--root", "whoami"])
        .assert()
        .success()
        .stdout(predicate::str::contains("root"));

    // Clean up
    let mut rm = Command::cargo_bin("devc").unwrap();
    apply_devc_env(&mut rm, &xdg);
    rm.args(["rm", container_name, "--force"])
        .assert()
        .success();
}

#[test]
#[ignore]
fn test_stop_and_remove() {
    // Placeholder — depends on a running container from test_build_and_up.
    eprintln!("Skipped: requires running container from prior test");
}
