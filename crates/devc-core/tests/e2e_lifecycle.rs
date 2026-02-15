//! End-to-end lifecycle tests for ContainerManager.
//!
//! These tests exercise the full lifecycle through ContainerManager with a real
//! container runtime. Each lifecycle command appends its name to /tmp/lifecycle.log
//! inside the container. After each phase, we exec `cat /tmp/lifecycle.log` and
//! verify the expected ordering.
//!
//! Requires Docker or Podman. Tests are `#[ignore]` and run explicitly.

use devc_config::GlobalConfig;
use devc_core::ContainerManager;
use devc_provider::{CliProvider, ContainerId, ContainerProvider, ExecConfig};
use std::path::Path;
use tempfile::TempDir;

/// Get a provider for testing (tries toolbox, podman, then docker)
async fn get_test_provider() -> Option<CliProvider> {
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

/// Read /tmp/lifecycle.log from the container, returning trimmed output.
async fn read_lifecycle_log(provider: &CliProvider, container_id: &str) -> String {
    let cid = ContainerId::new(container_id);
    let config = ExecConfig {
        cmd: vec!["cat".to_string(), "/tmp/lifecycle.log".to_string()],
        ..Default::default()
    };
    let result = provider
        .exec(&cid, &config)
        .await
        .expect("cat lifecycle.log");
    result.output.trim().to_string()
}

/// Create a workspace for the image-based lifecycle test.
/// Returns (tempdir, host_marker_path).
fn create_lifecycle_image_workspace(host_marker: &Path) -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let dc_dir = temp.path().join(".devcontainer");
    std::fs::create_dir_all(&dc_dir).expect("failed to create .devcontainer");

    // Use debian (not alpine) because Container::from_config() loads GlobalConfig
    // which defaults shell to /bin/bash — alpine doesn't have bash.
    let config = format!(
        r#"{{
            "image": "debian:bookworm-slim",
            "initializeCommand": "touch {marker}",
            "onCreateCommand": "echo on-create >> /tmp/lifecycle.log",
            "updateContentCommand": "echo update-content >> /tmp/lifecycle.log",
            "postCreateCommand": "echo post-create >> /tmp/lifecycle.log",
            "postStartCommand": "echo post-start >> /tmp/lifecycle.log",
            "postAttachCommand": "echo post-attach >> /tmp/lifecycle.log"
        }}"#,
        marker = host_marker.display()
    );
    std::fs::write(dc_dir.join("devcontainer.json"), config).expect("write devcontainer.json");
    temp
}

/// Create a workspace for the compose-based lifecycle test.
/// Returns (tempdir, host_marker_path).
fn create_lifecycle_compose_workspace(host_marker: &Path) -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let dc_dir = temp.path().join(".devcontainer");
    std::fs::create_dir_all(&dc_dir).expect("failed to create .devcontainer");

    let config = format!(
        r#"{{
            "dockerComposeFile": "docker-compose.yml",
            "service": "app",
            "initializeCommand": "touch {marker}",
            "onCreateCommand": "echo on-create >> /tmp/lifecycle.log",
            "updateContentCommand": "echo update-content >> /tmp/lifecycle.log",
            "postCreateCommand": "echo post-create >> /tmp/lifecycle.log",
            "postStartCommand": "echo post-start >> /tmp/lifecycle.log",
            "postAttachCommand": "echo post-attach >> /tmp/lifecycle.log"
        }}"#,
        marker = host_marker.display()
    );
    std::fs::write(dc_dir.join("devcontainer.json"), config).expect("write devcontainer.json");

    let compose = r#"services:
  app:
    image: alpine:latest
    command: sleep infinity
    tty: true
    stdin_open: true
"#;
    std::fs::write(dc_dir.join("docker-compose.yml"), compose).expect("write docker-compose.yml");
    temp
}

/// Get the container_id from the manager's state for a given devc id.
async fn get_container_id(mgr: &ContainerManager, id: &str) -> String {
    mgr.get(id)
        .await
        .expect("get state")
        .expect("state exists")
        .container_id
        .expect("container_id should be set")
}

/// Create a ContainerManager with credentials/SSH disabled and /bin/sh shell
/// (alpine doesn't have /bin/bash).
async fn create_test_manager(provider: CliProvider) -> ContainerManager {
    let mut config = GlobalConfig::default();
    config.credentials.docker = false;
    config.credentials.git = false;
    config.defaults.shell = "/bin/sh".to_string();
    ContainerManager::with_config(Box::new(provider), config)
        .await
        .expect("create manager")
}

/// Parse lifecycle log lines into a Vec<&str>.
fn parse_log_lines(log: &str) -> Vec<&str> {
    log.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect()
}

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_image_lifecycle_events() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Pull image first
    let _ = provider.pull("debian:bookworm-slim").await;

    let host_marker = std::env::temp_dir().join("devc_e2e_lifecycle_marker");
    let _ = std::fs::remove_file(&host_marker);

    let workspace = create_lifecycle_image_workspace(&host_marker);
    let config_path = workspace.path().join(".devcontainer/devcontainer.json");

    let mgr = create_test_manager(get_test_provider().await.unwrap()).await;

    // Phase 1: init + up (first create)
    let cs = mgr
        .init_from_config(&config_path)
        .await
        .expect("init")
        .expect("new state");
    let id = cs.id.clone();

    mgr.up(&id).await.expect("up should succeed");

    // Verify initializeCommand ran on host
    assert!(
        host_marker.exists(),
        "initializeCommand should create host marker"
    );

    // Verify lifecycle log
    let cid = get_container_id(&mgr, &id).await;
    let log = read_lifecycle_log(
        // We need a fresh provider instance for direct exec
        &get_test_provider().await.unwrap(),
        &cid,
    )
    .await;
    let lines = parse_log_lines(&log);
    assert_eq!(
        lines,
        vec!["on-create", "update-content", "post-create", "post-start"],
        "Phase 1: expected 4 lifecycle events in order, got: {:?}",
        lines
    );

    // Phase 2: postAttachCommand
    mgr.run_post_attach_command(&id).await.expect("post-attach");
    let log = read_lifecycle_log(&get_test_provider().await.unwrap(), &cid).await;
    let lines = parse_log_lines(&log);
    assert_eq!(
        lines.last().copied(),
        Some("post-attach"),
        "Phase 2: last line should be post-attach"
    );
    assert_eq!(lines.len(), 5, "Phase 2: should have 5 lines total");

    // Phase 3: stop + start (only postStart, no create-phase commands)
    mgr.stop(&id).await.expect("stop");
    mgr.start(&id).await.expect("start");

    // After restart the container is new, but for image-based containers
    // the old /tmp/lifecycle.log should persist (stop doesn't destroy container)
    let log = read_lifecycle_log(&get_test_provider().await.unwrap(), &cid).await;
    let lines = parse_log_lines(&log);
    assert_eq!(
        lines.last().copied(),
        Some("post-start"),
        "Phase 3: last line should be post-start after restart"
    );
    assert_eq!(
        lines.len(),
        6,
        "Phase 3: should have 6 lines (5 prior + 1 new post-start)"
    );
    // Verify no new on-create/update-content/post-create were added
    assert_eq!(
        lines.iter().filter(|&&l| l == "on-create").count(),
        1,
        "on-create should appear only once"
    );

    // Phase 4: rebuild (fresh container, fresh log)
    std::fs::remove_file(&host_marker).unwrap();
    mgr.rebuild(&id, false).await.expect("rebuild");
    assert!(
        host_marker.exists(),
        "initializeCommand should run during rebuild"
    );

    let new_cid = get_container_id(&mgr, &id).await;
    let log = read_lifecycle_log(&get_test_provider().await.unwrap(), &new_cid).await;
    let lines = parse_log_lines(&log);
    assert_eq!(
        lines,
        vec!["on-create", "update-content", "post-create", "post-start"],
        "Phase 4: rebuild should produce fresh lifecycle log"
    );

    // Phase 5: down
    mgr.down(&id).await.expect("down");
    let cs = mgr.get(&id).await.expect("get").expect("state exists");
    assert!(
        cs.container_id.is_none(),
        "container_id should be cleared after down"
    );

    // Clean up: remove from state
    mgr.remove(&id, true).await.ok();
    let _ = std::fs::remove_file(&host_marker);
}

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_compose_lifecycle_events() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Pull alpine first
    let _ = provider.pull("alpine:latest").await;

    let host_marker = std::env::temp_dir().join("devc_e2e_compose_lifecycle_marker");
    let _ = std::fs::remove_file(&host_marker);

    let workspace = create_lifecycle_compose_workspace(&host_marker);
    let config_path = workspace.path().join(".devcontainer/devcontainer.json");

    let mgr = create_test_manager(get_test_provider().await.unwrap()).await;

    // Phase 1: init + up
    let cs = mgr
        .init_from_config(&config_path)
        .await
        .expect("init")
        .expect("new state");
    let id = cs.id.clone();

    mgr.up(&id).await.expect("up should succeed");

    assert!(
        host_marker.exists(),
        "initializeCommand should create host marker"
    );

    let cid = get_container_id(&mgr, &id).await;
    let log = read_lifecycle_log(&get_test_provider().await.unwrap(), &cid).await;
    let lines = parse_log_lines(&log);
    assert_eq!(
        lines,
        vec!["on-create", "update-content", "post-create", "post-start"],
        "Phase 1: expected 4 lifecycle events in order, got: {:?}",
        lines
    );

    // Phase 2: postAttachCommand
    mgr.run_post_attach_command(&id).await.expect("post-attach");
    let log = read_lifecycle_log(&get_test_provider().await.unwrap(), &cid).await;
    let lines = parse_log_lines(&log);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[4], "post-attach");

    // Phase 3: stop + start (compose down/up cycle)
    // Compose stop does compose_down which destroys containers, so lifecycle.log
    // will be lost. After start (compose_up), only postStart runs.
    mgr.stop(&id).await.expect("stop");
    mgr.start(&id).await.expect("start");

    let new_cid = get_container_id(&mgr, &id).await;
    let log = read_lifecycle_log(&get_test_provider().await.unwrap(), &new_cid).await;
    let lines = parse_log_lines(&log);
    // After compose stop+start, container is recreated. Only post-start runs.
    assert_eq!(
        lines,
        vec!["post-start"],
        "Phase 3: compose restart should only run post-start in new container"
    );

    // Phase 4: rebuild
    std::fs::remove_file(&host_marker).unwrap();
    mgr.rebuild(&id, false).await.expect("rebuild");
    assert!(
        host_marker.exists(),
        "initializeCommand should run during rebuild"
    );

    let rebuilt_cid = get_container_id(&mgr, &id).await;
    let log = read_lifecycle_log(&get_test_provider().await.unwrap(), &rebuilt_cid).await;
    let lines = parse_log_lines(&log);
    assert_eq!(
        lines,
        vec!["on-create", "update-content", "post-create", "post-start"],
        "Phase 4: rebuild should produce fresh lifecycle log"
    );

    // Phase 5: down
    mgr.down(&id).await.expect("down");
    let cs = mgr.get(&id).await.expect("get").expect("state exists");
    assert!(
        cs.container_id.is_none(),
        "container_id should be cleared after down"
    );

    // Clean up
    mgr.remove(&id, true).await.ok();
    let _ = std::fs::remove_file(&host_marker);
}
