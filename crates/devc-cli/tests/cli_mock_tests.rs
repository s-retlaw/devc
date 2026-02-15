//! Mock-based command tests.
//!
//! These tests call command functions directly with a `ContainerManager`
//! backed by a `MockProvider`, avoiding any real container runtime.

use devc_cli::commands;
use devc_config::GlobalConfig;
use devc_core::test_support::MockProvider;
use devc_core::{ContainerState, DevcContainerStatus, StateStore};
use devc_provider::ProviderType;

/// Create a ContainerState with the given fields, pre-populated in the store.
/// Also creates a minimal devcontainer.json at the expected path so that
/// manager operations that load the config don't fail.
fn make_container(
    name: &str,
    status: DevcContainerStatus,
    container_id: Option<&str>,
    workspace: &std::path::Path,
) -> ContainerState {
    let devcontainer_dir = workspace.join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).ok();
    let config_path = devcontainer_dir.join("devcontainer.json");
    if !config_path.exists() {
        std::fs::write(&config_path, r#"{"image": "ubuntu:22.04"}"#).ok();
    }
    let mut cs = ContainerState::new(
        name.to_string(),
        ProviderType::Docker,
        config_path,
        workspace.to_path_buf(),
    );
    cs.status = status;
    cs.container_id = container_id.map(|s| s.to_string());
    cs
}

/// Build a ContainerManager backed by MockProvider with the given state.
fn test_manager(mock: MockProvider, store: StateStore) -> devc_core::ContainerManager {
    devc_core::ContainerManager::new_for_testing(Box::new(mock), GlobalConfig::default(), store)
}

/// Build a StateStore containing the given containers.
fn store_with(containers: Vec<ContainerState>) -> StateStore {
    let mut store = StateStore::new();
    for cs in containers {
        store.add(cs);
    }
    store
}

// ---- tests ----

#[tokio::test]
async fn test_start_already_running() {
    let tmp = tempfile::tempdir().unwrap();
    let cs = make_container(
        "myapp",
        DevcContainerStatus::Running,
        Some("cid123"),
        tmp.path(),
    );
    let name = cs.name.clone();
    let store = store_with(vec![cs]);
    let mock = MockProvider::new(ProviderType::Docker);
    let manager = test_manager(mock, store);

    // start on an already-running container should succeed (prints "already running")
    let result = commands::start(&manager, &name).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_stop_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let cs = make_container(
        "myapp",
        DevcContainerStatus::Stopped,
        Some("cid123"),
        tmp.path(),
    );
    let name = cs.name.clone();
    let store = store_with(vec![cs]);
    let mock = MockProvider::new(ProviderType::Docker);
    let manager = test_manager(mock, store);

    // stop on a non-running container should succeed (prints "not running")
    let result = commands::stop(&manager, &name).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_remove_force_running() {
    let tmp = tempfile::tempdir().unwrap();
    let cs = make_container(
        "myapp",
        DevcContainerStatus::Running,
        Some("cid123"),
        tmp.path(),
    );
    let name = cs.name.clone();
    let store = store_with(vec![cs]);
    let mock = MockProvider::new(ProviderType::Docker);
    let calls = mock.calls.clone();
    let manager = test_manager(mock, store);

    // force remove on a running container should succeed
    let result = commands::remove(&manager, &name, true).await;
    assert!(result.is_ok());

    // Verify that Remove was called on the provider
    let recorded = calls.lock().unwrap();
    assert!(
        recorded.iter().any(|c| matches!(
            c,
            devc_core::test_support::MockCall::Remove { force: true, .. }
        )),
        "Expected a Remove call with force=true, got: {:?}",
        *recorded,
    );
}

#[tokio::test]
async fn test_remove_no_force_running_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let cs = make_container(
        "myapp",
        DevcContainerStatus::Running,
        Some("cid123"),
        tmp.path(),
    );
    let name = cs.name.clone();
    let store = store_with(vec![cs]);
    let mock = MockProvider::new(ProviderType::Docker);
    let manager = test_manager(mock, store);

    // remove without force on a running container should fail
    let result = commands::remove(&manager, &name, false).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("cannot be removed") || err_msg.contains("--force"),
        "Expected error about force, got: {}",
        err_msg,
    );
}

#[tokio::test]
async fn test_list_with_containers() {
    let tmp = tempfile::tempdir().unwrap();
    let cs = make_container(
        "myapp",
        DevcContainerStatus::Running,
        Some("cid123"),
        tmp.path(),
    );
    let store = store_with(vec![cs]);
    let mock = MockProvider::new(ProviderType::Docker);
    let manager = test_manager(mock, store);

    let result = commands::list(&manager, false, false).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_list_empty() {
    let store = StateStore::new();
    let mock = MockProvider::new(ProviderType::Docker);
    let manager = test_manager(mock, store);

    // Should succeed and print "No containers found"
    let result = commands::list(&manager, false, false).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_down_calls_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let cs = make_container(
        "myapp",
        DevcContainerStatus::Running,
        Some("cid123"),
        tmp.path(),
    );
    let name = cs.name.clone();
    let store = store_with(vec![cs]);
    let mock = MockProvider::new(ProviderType::Docker);
    let calls = mock.calls.clone();
    let manager = test_manager(mock, store);

    let result = commands::down(&manager, &name).await;
    assert!(result.is_ok());

    // down should have called Stop on the provider
    let recorded = calls.lock().unwrap();
    assert!(
        recorded
            .iter()
            .any(|c| matches!(c, devc_core::test_support::MockCall::Stop { .. })),
        "Expected a Stop call, got: {:?}",
        *recorded,
    );
}

#[tokio::test]
async fn test_config_shows_defaults() {
    // config(false) should succeed -- it reads/prints the config file
    let result = commands::config(false).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_find_container_by_name_through_start() {
    // Tests that find_container resolves by name (not just by ID)
    let tmp = tempfile::tempdir().unwrap();
    let cs = make_container(
        "my-unique-name",
        DevcContainerStatus::Stopped,
        Some("cid456"),
        tmp.path(),
    );
    let store = store_with(vec![cs]);
    let mock = MockProvider::new(ProviderType::Docker);
    // Set mock inspect to return Exited so that provider.start() is actually called
    *mock.inspect_result.lock().unwrap() = Ok(devc_core::test_support::mock_container_details(
        "cid456",
        devc_provider::ContainerStatus::Exited,
    ));
    let calls = mock.calls.clone();
    let manager = test_manager(mock, store);

    // Start by name should find the container and call start
    let result = commands::start(&manager, "my-unique-name").await;
    assert!(result.is_ok());

    let recorded = calls.lock().unwrap();
    assert!(
        recorded
            .iter()
            .any(|c| matches!(c, devc_core::test_support::MockCall::Inspect { .. })),
        "Expected an Inspect call, got: {:?}",
        *recorded,
    );
    assert!(
        recorded
            .iter()
            .any(|c| matches!(c, devc_core::test_support::MockCall::Start { .. })),
        "Expected a Start call, got: {:?}",
        *recorded,
    );
}

#[tokio::test]
async fn test_find_container_not_found() {
    let store = StateStore::new();
    let mock = MockProvider::new(ProviderType::Docker);
    let manager = test_manager(mock, store);

    let result = commands::start(&manager, "nonexistent").await;
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("not found"),
        "Expected 'not found' error",
    );
}
