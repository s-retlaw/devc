//! End-to-end tests for port forwarding configuration and Docker Compose integration.
//!
//! Test A runs without Docker (config-only parsing).
//! Tests B and C require a container runtime (Docker or Podman) and are `#[ignore]`.

use devc_config::{AutoForwardAction, PortForwardConfig};
use devc_core::Container;
use devc_provider::{CliProvider, ContainerProvider, ExecConfig};
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

/// Create a workspace with devcontainer.json and a docker-compose.yml
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

/// Pull alpine:latest, skipping if already present
async fn ensure_alpine(provider: &CliProvider) {
    let _ = provider.pull("alpine:latest").await;
}

/// Helper to build a PortForwardConfig concisely
fn pfc(
    port: u16,
    action: AutoForwardAction,
    label: Option<&str>,
    protocol: Option<&str>,
) -> PortForwardConfig {
    PortForwardConfig {
        port,
        action,
        label: label.map(String::from),
        protocol: protocol.map(String::from),
    }
}

// ========================================================================
// Test A: Config-only test — no Docker required
// ========================================================================

#[tokio::test]
async fn test_e2e_auto_forward_config_full_spec() {
    let workspace = create_test_workspace(
        r#"{
            "image": "alpine:latest",
            "forwardPorts": [
                3000,
                {"port": 8080, "label": "API", "protocol": "https", "onAutoForward": "silent"},
                {"port": 9090, "onAutoForward": "openBrowser"}
            ],
            "appPort": [4000, 5000],
            "portsAttributes": {
                "3000": {"label": "Frontend", "protocol": "http", "onAutoForward": "openBrowserOnce"},
                "5000": {"label": "Metrics"},
                "6000": {"label": "Debug", "onAutoForward": "ignore"}
            }
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");
    let fwd = container.devcontainer.auto_forward_config();

    // forwardPorts: 3000 (numeric), 8080 (object), 9090 (object)
    // appPort: 4000, 5000
    // portsAttributes: overrides 3000 and 5000, adds 6000

    // 3000: starts as Notify from numeric forwardPorts, overridden by portsAttributes
    assert!(
        fwd.iter().any(|p| *p
            == pfc(
                3000,
                AutoForwardAction::OpenBrowserOnce,
                Some("Frontend"),
                Some("http")
            )),
        "port 3000 should be overridden by portsAttributes: {:?}",
        fwd.iter().find(|p| p.port == 3000)
    );

    // 8080: object in forwardPorts with label/protocol/action
    assert!(
        fwd.iter()
            .any(|p| *p == pfc(8080, AutoForwardAction::Silent, Some("API"), Some("https"))),
        "port 8080 should have label=API, protocol=https, action=Silent: {:?}",
        fwd.iter().find(|p| p.port == 8080)
    );

    // 9090: object in forwardPorts with openBrowser
    assert!(
        fwd.iter()
            .any(|p| *p == pfc(9090, AutoForwardAction::OpenBrowser, None, None)),
        "port 9090 should be openBrowser: {:?}",
        fwd.iter().find(|p| p.port == 9090)
    );

    // 4000: from appPort, always Silent, no portsAttributes override
    assert!(
        fwd.iter()
            .any(|p| *p == pfc(4000, AutoForwardAction::Silent, None, None)),
        "port 4000 should be Silent from appPort: {:?}",
        fwd.iter().find(|p| p.port == 4000)
    );

    // 5000: from appPort (Silent), portsAttributes adds label
    assert!(
        fwd.iter()
            .any(|p| *p == pfc(5000, AutoForwardAction::Silent, Some("Metrics"), None)),
        "port 5000 should have label=Metrics from portsAttributes: {:?}",
        fwd.iter().find(|p| p.port == 5000)
    );

    // 6000: only in portsAttributes, added as new entry
    assert!(
        fwd.iter()
            .any(|p| *p == pfc(6000, AutoForwardAction::Ignore, Some("Debug"), None)),
        "port 6000 should be added from portsAttributes: {:?}",
        fwd.iter().find(|p| p.port == 6000)
    );

    // Total: 3 from forwardPorts + 2 from appPort + 1 new from portsAttributes = 6
    assert_eq!(fwd.len(), 6, "should have 6 port forward configs");
}

// ========================================================================
// Test B: Compose lifecycle with port config
// ========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_compose_lifecycle_with_port_config() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    ensure_alpine(&provider).await;

    let workspace = create_compose_workspace(
        r#"{
            "dockerComposeFile": "docker-compose.yml",
            "service": "app",
            "forwardPorts": [
                {"port": 3000, "label": "App Server"},
                {"port": 5432, "label": "Database", "onAutoForward": "silent"}
            ]
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
        Container::from_workspace(workspace.path()).expect("should load container config");
    assert!(container.is_compose(), "should be compose project");
    assert_eq!(container.compose_service(), Some("app"));

    let compose_files = container
        .compose_files()
        .expect("should have compose files");
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

    // List services and verify both are running
    let services = provider
        .compose_ps(&compose_file_strs, &project_name, project_dir)
        .await
        .expect("compose_ps should succeed");

    assert!(
        services.len() >= 2,
        "should have at least 2 services, got: {:?}",
        services
    );

    // Find the app service container
    let app_service = services
        .iter()
        .find(|s| s.service_name.contains("app"))
        .expect("should find app service");

    // Exec into app to verify it's alive
    let exec = ExecConfig {
        cmd: vec!["echo".to_string(), "alive".to_string()],
        ..Default::default()
    };
    let result = provider
        .exec(&app_service.container_id, &exec)
        .await
        .expect("exec into app should work");
    assert!(
        result.output.contains("alive"),
        "app should respond: {}",
        result.output.trim()
    );

    // Verify auto_forward_config from loaded config
    let fwd = container.devcontainer.auto_forward_config();
    assert_eq!(fwd.len(), 2, "should have 2 port forward configs");
    assert!(fwd
        .iter()
        .any(|p| p.port == 3000 && p.label.as_deref() == Some("App Server")));
    assert!(fwd.iter().any(|p| p.port == 5432
        && p.label.as_deref() == Some("Database")
        && p.action == AutoForwardAction::Silent));

    // Cleanup
    let _ = provider
        .compose_down(&compose_file_strs, &project_name, project_dir)
        .await;
}

// ========================================================================
// Test C: Compose port detection via netcat
// ========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_compose_port_detection() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
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
        // The app service starts a netcat listener on port 3000
        r#"
services:
  app:
    image: alpine:latest
    command: ["sh", "-c", "nc -lk -p 3000 -e cat &\nsleep infinity"]
  db:
    image: alpine:latest
    command: ["sleep", "infinity"]
"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");
    let compose_files = container
        .compose_files()
        .expect("should have compose files");
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

    // Give netcat a moment to start listening
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Find the app service
    let services = provider
        .compose_ps(&compose_file_strs, &project_name, project_dir)
        .await
        .expect("compose_ps should succeed");

    let app_service = services
        .iter()
        .find(|s| s.service_name.contains("app"))
        .expect("should find app service");

    // Verify netcat is listening by probing the port from inside the container
    let exec = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo test | nc -w1 localhost 3000".to_string(),
        ],
        ..Default::default()
    };
    let result = provider
        .exec(&app_service.container_id, &exec)
        .await
        .expect("nc probe should work");
    assert_eq!(
        result.exit_code,
        0,
        "nc probe should succeed (port 3000 is listening), output: {}",
        result.output.trim()
    );

    // Cleanup
    let _ = provider
        .compose_down(&compose_file_strs, &project_name, project_dir)
        .await;
}
