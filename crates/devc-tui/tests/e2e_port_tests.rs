//! End-to-end tests for TUI-layer port detection and socat forwarding.
//!
//! All tests require a container runtime (Docker or Podman) and are `#[ignore]`.

use devc_provider::{CliProvider, ContainerProvider, CreateContainerConfig, ExecConfig};
use devc_tui::tunnel::spawn_forwarder;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Get a provider for testing (tries toolbox, podman, then docker).
/// Used for tests that only need the provider's exec method.
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

/// Get a provider whose type can be used with spawn_forwarder.
///
/// spawn_forwarder runs `docker exec` / `podman exec` directly (not through
/// the provider), so it doesn't work with toolbox's `flatpak-spawn --host`
/// indirection. This helper tries Docker first (works everywhere), then
/// Podman (works outside toolbox), skipping toolbox.
async fn get_direct_provider() -> Option<CliProvider> {
    if let Ok(p) = CliProvider::new_docker().await {
        return Some(p);
    }
    // Only use podman if we're NOT in a toolbox
    if !std::path::Path::new("/run/.containerenv").exists() {
        if let Ok(p) = CliProvider::new_podman().await {
            return Some(p);
        }
    }
    None
}

/// Pull alpine:latest, skipping if already present
async fn ensure_alpine(provider: &CliProvider) {
    let _ = provider.pull("alpine:latest").await;
}

/// Install socat via the provider's exec method (works in all environments).
async fn install_socat_via_exec(provider: &CliProvider, id: &devc_provider::ContainerId) {
    let exec = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            "apk add --no-cache socat".to_string(),
        ],
        user: Some("root".to_string()),
        ..Default::default()
    };
    let result = provider
        .exec(id, &exec)
        .await
        .expect("socat install should work");
    assert_eq!(
        result.exit_code, 0,
        "socat install failed: {}",
        result.output.trim()
    );
}

/// Create a workspace with devcontainer.json and docker-compose.yml
fn create_compose_workspace(devcontainer_json: &str, compose_yaml: &str) -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let devcontainer_dir = temp.path().join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).expect("failed to create .devcontainer dir");
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        devcontainer_json,
    )
    .expect("failed to write devcontainer.json");
    std::fs::write(devcontainer_dir.join("docker-compose.yml"), compose_yaml)
        .expect("failed to write docker-compose.yml");
    temp
}

// ========================================================================
// Test D: Port detection in a real container
// ========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_port_detection_real_container() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    ensure_alpine(&provider).await;

    let config = CreateContainerConfig {
        image: "alpine:latest".to_string(),
        name: Some("devc_test_port_detect".to_string()),
        cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
        tty: true,
        stdin_open: true,
        ..Default::default()
    };

    let _ = provider.remove_by_name("devc_test_port_detect").await;
    let id = provider.create(&config).await.expect("create should succeed");
    provider.start(&id).await.expect("start should succeed");

    // Start a netcat listener on port 3000 in the background
    let exec = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            "nc -lk -p 3000 &".to_string(),
        ],
        ..Default::default()
    };
    provider
        .exec(&id, &exec)
        .await
        .expect("nc start should work");

    // Give netcat time to bind
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Use the detect_ports function from devc-tui
    let ports = devc_tui::ports::detect_ports(&provider, &id)
        .await
        .expect("detect_ports should work");

    assert!(
        ports.contains(&3000),
        "should detect port 3000, got: {:?}",
        ports
    );

    // Cleanup
    let _ = provider.remove(&id, true).await;
}

// ========================================================================
// Test E: socat forwarding roundtrip
// ========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_socat_forwarding_roundtrip() {
    // spawn_forwarder needs a direct CLI provider (not toolbox)
    let provider = match get_direct_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no direct container runtime available (toolbox-only?)");
            return;
        }
    };

    ensure_alpine(&provider).await;

    let config = CreateContainerConfig {
        image: "alpine:latest".to_string(),
        name: Some("devc_test_socat_fwd".to_string()),
        cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
        tty: true,
        stdin_open: true,
        ..Default::default()
    };

    let _ = provider.remove_by_name("devc_test_socat_fwd").await;
    let id = provider.create(&config).await.expect("create should succeed");
    provider.start(&id).await.expect("start should succeed");

    // Install socat via provider exec (works in all environments)
    install_socat_via_exec(&provider, &id).await;

    // Start a socat echo server on port 4000
    let exec = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            "socat TCP-LISTEN:4000,fork,reuseaddr EXEC:cat &".to_string(),
        ],
        ..Default::default()
    };
    provider
        .exec(&id, &exec)
        .await
        .expect("socat start should work");

    // Give socat time to start
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Spawn forwarder: localhost:14000 -> container:4000
    let provider_type = provider.info().provider_type;
    let forwarder = spawn_forwarder(provider_type, &id.0, 14000, 4000)
        .await
        .expect("spawn_forwarder should succeed");

    assert!(forwarder.is_running(), "forwarder should be running");

    // Connect via TCP and do a roundtrip
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:14000")
        .await
        .expect("should connect to forwarder");

    stream
        .write_all(b"hello\n")
        .await
        .expect("write should succeed");

    // Read the echo back
    let mut buf = vec![0u8; 64];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("read should not timeout")
        .expect("read should succeed");

    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(
        response.contains("hello"),
        "should echo back 'hello', got: {:?}",
        response
    );

    // Stop forwarder and verify port is released
    forwarder.stop().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let port_available = std::net::TcpListener::bind("127.0.0.1:14000").is_ok();
    assert!(port_available, "port 14000 should be released after stop");

    // Cleanup
    let _ = provider.remove(&id, true).await;
}

// ========================================================================
// Test F: Compose with socat forwarding
// ========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_compose_with_socat_forwarding() {
    // spawn_forwarder needs a direct CLI provider (not toolbox)
    let provider = match get_direct_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no direct container runtime available (toolbox-only?)");
            return;
        }
    };

    ensure_alpine(&provider).await;

    let workspace = create_compose_workspace(
        r#"{
            "dockerComposeFile": "docker-compose.yml",
            "service": "app",
            "forwardPorts": [3000]
        }"#,
        r#"
services:
  app:
    image: alpine:latest
    command: ["sleep", "infinity"]
  db:
    image: alpine:latest
    command: ["sleep", "infinity"]
"#,
    );

    let container =
        devc_core::Container::from_workspace(workspace.path()).expect("should load config");
    let compose_files = container.compose_files().expect("should have compose files");
    let compose_file_strs: Vec<&str> = compose_files.iter().map(|p| p.to_str().unwrap()).collect();
    let project_name = container.compose_project_name();
    let project_dir = container.config_path.parent().unwrap();

    // Clean up any previous run
    let _ = provider
        .compose_down(&compose_file_strs, &project_name, project_dir)
        .await;

    // Start compose services
    provider
        .compose_up(&compose_file_strs, &project_name, project_dir, None)
        .await
        .expect("compose_up should succeed");

    // Find app service container
    let services = provider
        .compose_ps(&compose_file_strs, &project_name, project_dir)
        .await
        .expect("compose_ps should succeed");

    let app_service = services
        .iter()
        .find(|s| s.service_name.contains("app"))
        .expect("should find app service");
    let app_id = &app_service.container_id;

    // Install socat via provider exec
    install_socat_via_exec(&provider, app_id).await;

    // Start socat echo server on port 3000
    let exec = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            "socat TCP-LISTEN:3000,fork,reuseaddr EXEC:cat &".to_string(),
        ],
        ..Default::default()
    };
    provider
        .exec(app_id, &exec)
        .await
        .expect("socat start should work");

    // Give socat time to start
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Spawn forwarder: localhost:13000 -> container:3000
    let provider_type = provider.info().provider_type;
    let forwarder = spawn_forwarder(provider_type, &app_id.0, 13000, 3000)
        .await
        .expect("spawn_forwarder should succeed");

    // TCP roundtrip test
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:13000")
        .await
        .expect("should connect to forwarder");

    stream
        .write_all(b"compose-test\n")
        .await
        .expect("write should succeed");

    let mut buf = vec![0u8; 64];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("read should not timeout")
        .expect("read should succeed");

    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(
        response.contains("compose-test"),
        "should echo back 'compose-test', got: {:?}",
        response
    );

    // Stop forwarder
    forwarder.stop().await;

    // Cleanup
    let _ = provider
        .compose_down(&compose_file_strs, &project_name, project_dir)
        .await;
}
