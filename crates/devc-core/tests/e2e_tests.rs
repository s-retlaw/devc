//! End-to-end integration tests for devcontainer spec compliance.
//!
//! These tests spin up real containers to verify that runtime flags,
//! variable substitution, lifecycle hooks, and remoteEnv work correctly.
//!
//! Requires a container runtime (Docker or Podman) to be available.
//! Tests skip gracefully if no runtime is detected.

use devc_config::{Command, StringOrArray};
use devc_core::{run_host_command, run_lifecycle_command_with_env, Container};
use devc_provider::{
    CliProvider, ContainerId, ContainerProvider, CreateContainerConfig, ExecConfig,
};
use std::collections::HashMap;
use tempfile::TempDir;

/// Get a provider for testing.
///
/// Respects `DEVC_TEST_PROVIDER` env var (`docker`, `podman`, `toolbox`).
/// Falls back to first available runtime when unset.
async fn get_test_provider() -> Option<CliProvider> {
    match std::env::var("DEVC_TEST_PROVIDER").as_deref() {
        Ok("docker") => CliProvider::new_docker().await.ok(),
        Ok("podman") => CliProvider::new_podman().await.ok(),
        Ok("toolbox") => CliProvider::new_toolbox().await.ok(),
        _ => {
            if let Ok(p) = CliProvider::new_toolbox().await {
                return Some(p);
            }
            if let Ok(p) = CliProvider::new_podman().await {
                return Some(p);
            }
            if let Ok(p) = CliProvider::new_docker().await {
                return Some(p);
            }
            None
        }
    }
}

/// Create a temporary workspace with a devcontainer.json
fn create_test_workspace(devcontainer_json: &str) -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let devcontainer_dir = temp.path().join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).expect("failed to create .devcontainer dir");
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        devcontainer_json,
    )
    .expect("failed to write devcontainer.json");
    temp
}

/// Inspect a running container and return a single formatted field.
/// Handles direct runtime access and Toolbox (flatpak-spawn --host) environments.
fn inspect_container_field(provider: &CliProvider, cid: &ContainerId, format: &str) -> String {
    let runtime = provider.info().provider_type.to_string();
    let args = ["inspect", "--format", format, &cid.0];

    // Try direct command first
    if let Ok(output) = std::process::Command::new(&runtime).args(&args).output() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }

    // Fallback: try via flatpak-spawn (Toolbox environments)
    if let Ok(output) = std::process::Command::new("flatpak-spawn")
        .arg("--host")
        .arg(&runtime)
        .args(&args)
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }

    panic!(
        "{runtime} inspect returned empty for container {} with format {format}",
        cid.0
    );
}

/// Pull alpine:latest, skipping if already present
async fn ensure_alpine(provider: &CliProvider) {
    let _ = provider.pull("alpine:latest").await;
}

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_runtime_flags() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    let workspace = create_test_workspace(
        r#"{
            "image": "alpine:latest",
            "init": true,
            "capAdd": ["SYS_PTRACE"],
            "securityOpt": ["seccomp=unconfined"],
            "containerEnv": {"MY_VAR": "hello"},
            "remoteEnv": {"EDITOR": "vim"}
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Verify create_config fields
    let config = container.create_config("alpine:latest");
    assert!(config.init, "init should be true");
    assert_eq!(config.cap_add, vec!["SYS_PTRACE"]);
    assert_eq!(config.security_opt, vec!["seccomp=unconfined"]);
    assert!(
        config
            .env
            .get("MY_VAR")
            .map(|v| v == "hello")
            .unwrap_or(false),
        "containerEnv MY_VAR should be in create config"
    );
    // remoteEnv should NOT be in create config
    assert!(
        !config.env.contains_key("EDITOR"),
        "remoteEnv should NOT be in create config"
    );

    // Actually create and start the container
    ensure_alpine(&provider).await;
    // Override cmd to use /bin/sh (alpine doesn't have bash)
    let mut config = config;
    config.cmd = Some(vec![
        "sh".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);
    let _ = provider.remove_by_name(&config.name.clone().unwrap()).await;

    let id = provider
        .create(&config)
        .await
        .expect("create should succeed");
    provider.start(&id).await.expect("start should succeed");

    // Verify containerEnv via exec
    let exec = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo $MY_VAR".to_string(),
        ],
        ..Default::default()
    };
    let result = provider.exec(&id, &exec).await.expect("exec should work");
    assert!(
        result.output.contains("hello"),
        "MY_VAR should be 'hello', got: {}",
        result.output.trim()
    );

    // Verify remoteEnv via exec_config (which includes remoteEnv)
    let exec_with_remote = container.exec_config(
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo $EDITOR".to_string(),
        ],
        false,
        false,
    );
    let result = provider
        .exec(&id, &exec_with_remote)
        .await
        .expect("exec with remoteEnv should work");
    assert!(
        result.output.contains("vim"),
        "EDITOR should be 'vim' via remoteEnv, got: {}",
        result.output.trim()
    );

    // Verify CapAdd is actually set on the running container
    let cap_add_json = inspect_container_field(&provider, &id, "{{json .HostConfig.CapAdd}}");
    eprintln!("Container CapAdd: {}", cap_add_json);
    assert!(
        cap_add_json.contains("SYS_PTRACE"),
        "Container should have SYS_PTRACE capability, inspect output: {}",
        cap_add_json
    );

    // Verify SecurityOpt is actually set on the running container
    let sec_opt_json = inspect_container_field(&provider, &id, "{{json .HostConfig.SecurityOpt}}");
    eprintln!("Container SecurityOpt: {}", sec_opt_json);
    assert!(
        sec_opt_json.contains("seccomp=unconfined"),
        "Container should have seccomp=unconfined, inspect output: {}",
        sec_opt_json
    );

    // Verify Init is actually set on the running container
    let init_json = inspect_container_field(&provider, &id, "{{json .HostConfig.Init}}");
    eprintln!("Container Init: {}", init_json);
    assert!(
        init_json.contains("true"),
        "Container should have init=true, inspect output: {}",
        init_json
    );

    // Cleanup
    let _ = provider.remove(&id, true).await;
}

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_variable_substitution() {
    let workspace = create_test_workspace(
        r#"{
            "image": "alpine:latest",
            "containerEnv": {
                "HOST_PATH": "${localWorkspaceFolder}",
                "CONTAINER_PATH": "${containerWorkspaceFolder}"
            },
            "workspaceFolder": "/workspace"
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Verify substitution happened
    let env = container.devcontainer.container_env.as_ref().unwrap();
    let host_path = env.get("HOST_PATH").unwrap();
    assert!(
        !host_path.contains("${localWorkspaceFolder}"),
        "HOST_PATH should be substituted, got: {}",
        host_path
    );
    assert_eq!(
        host_path,
        &workspace.path().to_string_lossy().to_string(),
        "HOST_PATH should match workspace path"
    );

    let container_path = env.get("CONTAINER_PATH").unwrap();
    assert_eq!(
        container_path, "/workspace",
        "CONTAINER_PATH should be /workspace"
    );

    // Also verify via real container
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping container verification: no runtime available");
            return;
        }
    };

    ensure_alpine(&provider).await;
    let mut config = container.create_config("alpine:latest");
    // Override cmd to use /bin/sh (alpine doesn't have bash)
    config.cmd = Some(vec![
        "sh".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);
    let _ = provider.remove_by_name(&config.name.clone().unwrap()).await;

    let id = provider.create(&config).await.expect("create");
    provider.start(&id).await.expect("start");

    let exec = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo $CONTAINER_PATH".to_string(),
        ],
        ..Default::default()
    };
    let result = provider.exec(&id, &exec).await.expect("exec");
    assert!(
        result.output.contains("/workspace"),
        "CONTAINER_PATH inside container should be /workspace, got: {}",
        result.output.trim()
    );

    let _ = provider.remove(&id, true).await;
}

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_lifecycle_hooks() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    let workspace = create_test_workspace(
        r#"{
            "image": "alpine:latest",
            "onCreateCommand": "touch /tmp/oncreate_ran",
            "postCreateCommand": "touch /tmp/postcreate_ran",
            "postStartCommand": "touch /tmp/poststart_ran"
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    ensure_alpine(&provider).await;
    let mut config = container.create_config("alpine:latest");
    // Override cmd to use /bin/sh (alpine doesn't have bash)
    config.cmd = Some(vec![
        "sh".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);
    let _ = provider.remove_by_name(&config.name.clone().unwrap()).await;

    let id = provider.create(&config).await.expect("create");
    provider.start(&id).await.expect("start");

    // Run lifecycle hooks
    if let Some(ref cmd) = container.devcontainer.on_create_command {
        run_lifecycle_command_with_env(&provider, &id, cmd, None, None, None)
            .await
            .expect("onCreateCommand should succeed");
    }
    if let Some(ref cmd) = container.devcontainer.post_create_command {
        run_lifecycle_command_with_env(&provider, &id, cmd, None, None, None)
            .await
            .expect("postCreateCommand should succeed");
    }
    if let Some(ref cmd) = container.devcontainer.post_start_command {
        run_lifecycle_command_with_env(&provider, &id, cmd, None, None, None)
            .await
            .expect("postStartCommand should succeed");
    }

    // Verify all marker files exist
    let exec = ExecConfig {
        cmd: vec![
            "ls".to_string(),
            "/tmp/oncreate_ran".to_string(),
            "/tmp/postcreate_ran".to_string(),
            "/tmp/poststart_ran".to_string(),
        ],
        ..Default::default()
    };
    let result = provider.exec(&id, &exec).await.expect("exec ls");
    assert_eq!(
        result.exit_code,
        0,
        "All lifecycle marker files should exist, output: {}",
        result.output.trim()
    );

    let _ = provider.remove(&id, true).await;
}

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_parallel_object_commands() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    ensure_alpine(&provider).await;

    // Create a minimal container
    let config = CreateContainerConfig {
        image: "alpine:latest".to_string(),
        name: Some("devc_test_parallel_cmds".to_string()),
        cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
        tty: true,
        stdin_open: true,
        ..Default::default()
    };

    let _ = provider.remove_by_name("devc_test_parallel_cmds").await;
    let id = provider.create(&config).await.expect("create");
    provider.start(&id).await.expect("start");

    // Build an Object command with 3 named commands
    let mut commands = HashMap::new();
    commands.insert(
        "a".to_string(),
        StringOrArray::String("touch /tmp/cmd_a".to_string()),
    );
    commands.insert(
        "b".to_string(),
        StringOrArray::String("touch /tmp/cmd_b".to_string()),
    );
    commands.insert(
        "c".to_string(),
        StringOrArray::String("touch /tmp/cmd_c".to_string()),
    );
    let cmd = Command::Object(commands);

    run_lifecycle_command_with_env(&provider, &id, &cmd, None, None, None)
        .await
        .expect("parallel object commands should succeed");

    // Verify all marker files exist
    let exec = ExecConfig {
        cmd: vec![
            "ls".to_string(),
            "/tmp/cmd_a".to_string(),
            "/tmp/cmd_b".to_string(),
            "/tmp/cmd_c".to_string(),
        ],
        ..Default::default()
    };
    let result = provider.exec(&id, &exec).await.expect("exec ls");
    assert_eq!(
        result.exit_code,
        0,
        "All parallel command marker files should exist, output: {}",
        result.output.trim()
    );

    let _ = provider.remove(&id, true).await;
}

#[tokio::test]
async fn test_e2e_host_command() {
    let temp = TempDir::new().expect("temp dir");
    let marker = temp.path().join("host_ran");

    // Test successful host command
    let cmd = Command::String(format!("touch {}", marker.display()));
    run_host_command(&cmd, temp.path(), None)
        .await
        .expect("host command should succeed");
    assert!(marker.exists(), "Marker file should exist on host");

    // Test failure case
    let fail_cmd = Command::String("false".to_string());
    let result = run_host_command(&fail_cmd, temp.path(), None).await;
    assert!(result.is_err(), "Command 'false' should fail");
}

#[tokio::test]
async fn test_e2e_remote_env_not_in_create() {
    let workspace = create_test_workspace(
        r#"{
            "image": "alpine:latest",
            "remoteEnv": {"SECRET": "hunter2"},
            "containerEnv": {"PUBLIC": "visible"}
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // create_config should NOT include remoteEnv
    let create = container.create_config("alpine:latest");
    assert!(
        !create.env.contains_key("SECRET"),
        "remoteEnv SECRET should NOT be in create config"
    );
    assert!(
        create
            .env
            .get("PUBLIC")
            .map(|v| v == "visible")
            .unwrap_or(false),
        "containerEnv PUBLIC should be in create config"
    );

    // exec_config SHOULD include remoteEnv
    let exec = container.exec_config(vec!["echo".to_string()], false, false);
    assert_eq!(
        exec.env.get("SECRET").unwrap(),
        "hunter2",
        "remoteEnv SECRET should be in exec config"
    );
    assert!(
        exec.env
            .get("PUBLIC")
            .map(|v| v == "visible")
            .unwrap_or(false),
        "containerEnv PUBLIC should also be in exec config"
    );
}
