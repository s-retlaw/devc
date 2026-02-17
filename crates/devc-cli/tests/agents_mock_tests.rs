use devc_config::GlobalConfig;
use devc_core::test_support::{MockCall, MockProvider};
use devc_core::{ContainerManager, ContainerState, DevcContainerStatus, StateStore};
use devc_provider::ProviderType;

fn make_running_container(workspace: &std::path::Path) -> ContainerState {
    let devcontainer_dir = workspace.join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).unwrap();
    let config_path = devcontainer_dir.join("devcontainer.json");
    std::fs::write(
        &config_path,
        r#"{"image":"ubuntu:22.04","remoteUser":"root"}"#,
    )
    .unwrap();

    let mut cs = ContainerState::new(
        "agent-test".to_string(),
        ProviderType::Docker,
        config_path,
        workspace.to_path_buf(),
    );
    cs.status = DevcContainerStatus::Running;
    cs.container_id = Some("cid-agent-test".to_string());
    cs
}

fn manager_with(
    mock: MockProvider,
    config: GlobalConfig,
    container: ContainerState,
) -> ContainerManager {
    let mut state = StateStore::new();
    state.add(container);
    ContainerManager::new_for_testing(Box::new(mock), config, state)
}

#[tokio::test]
async fn test_agent_sync_enabled_triggers_copy_and_install() {
    let tmp = tempfile::tempdir().unwrap();
    let host_dir = tmp.path().join("codex");
    std::fs::create_dir_all(&host_dir).unwrap();
    std::fs::write(host_dir.join("auth.json"), "{}").unwrap();

    let mut config = GlobalConfig::default();
    config.agents.codex.enabled = Some(true);
    config.agents.claude.enabled = Some(false);
    config.agents.cursor.enabled = Some(false);
    config.agents.gemini.enabled = Some(false);
    config.agents.codex.host_config_path = Some(host_dir.display().to_string());
    config.agents.codex.container_config_path = Some("/tmp/.codex".to_string());
    config.agents.codex.install_command = Some("echo install-codex".to_string());

    let mock = MockProvider::new(ProviderType::Docker);
    let calls = mock.calls.clone();
    mock.exec_responses
        .lock()
        .unwrap()
        .push((0, "/root".to_string())); // HOME probe
    mock.exec_responses
        .lock()
        .unwrap()
        .push((0, "root".to_string())); // user probe
    mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir
    mock.exec_responses.lock().unwrap().push((0, String::new())); // chown
    mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod
    mock.exec_responses.lock().unwrap().push((0, String::new())); // PATH bootstrap
    mock.exec_responses.lock().unwrap().push((1, String::new())); // probe missing
    mock.exec_responses.lock().unwrap().push((0, String::new())); // node/npm present
    mock.exec_responses.lock().unwrap().push((0, String::new())); // install ok
    mock.exec_responses.lock().unwrap().push((0, String::new())); // post-install probe

    let container = make_running_container(tmp.path());
    let id = container.id.clone();
    let manager = manager_with(mock, config, container);

    let results = manager.setup_agents_for_container(&id).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].warnings.is_empty());
    assert!(results[0].copied);
    assert!(results[0].installed);

    let recorded = calls.lock().unwrap();
    assert!(
        recorded
            .iter()
            .any(|c| matches!(c, MockCall::CopyInto { .. })),
        "expected copy_into call, got: {:?}",
        *recorded
    );
    assert!(
        recorded.iter().any(
            |c| matches!(c, MockCall::Exec { cmd, .. } if cmd.iter().any(|p| p.contains("command -v codex")))
        ),
        "expected codex probe exec call, got: {:?}",
        *recorded
    );
    assert!(
        recorded.iter().any(
            |c| matches!(c, MockCall::Exec { cmd, .. } if cmd.iter().any(|p| p.contains("install-codex")))
        ),
        "expected install exec call, got: {:?}",
        *recorded
    );
}

#[tokio::test]
async fn test_agent_sync_disabled_makes_no_provider_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = GlobalConfig::default();
    config.agents.codex.enabled = Some(false);
    config.agents.claude.enabled = Some(false);
    config.agents.cursor.enabled = Some(false);
    config.agents.gemini.enabled = Some(false);
    let mock = MockProvider::new(ProviderType::Docker);
    let calls = mock.calls.clone();
    let container = make_running_container(tmp.path());
    let id = container.id.clone();
    let manager = manager_with(mock, config, container);

    let results = manager.setup_agents_for_container(&id).await.unwrap();
    assert!(results.is_empty());
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_agent_sync_missing_host_prereq_returns_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = GlobalConfig::default();
    config.agents.codex.enabled = Some(true);
    config.agents.claude.enabled = Some(false);
    config.agents.cursor.enabled = Some(false);
    config.agents.gemini.enabled = Some(false);
    config.agents.codex.host_config_path = Some("/tmp/devc-missing-agent-host-config".to_string());

    let mock = MockProvider::new(ProviderType::Docker);
    mock.exec_responses
        .lock()
        .unwrap()
        .push((0, "/root".to_string())); // HOME probe
    mock.exec_responses
        .lock()
        .unwrap()
        .push((0, "root".to_string())); // user probe
    let calls = mock.calls.clone();
    let container = make_running_container(tmp.path());
    let id = container.id.clone();
    let manager = manager_with(mock, config, container);

    let results = manager.setup_agents_for_container(&id).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0].warnings.is_empty());

    let recorded = calls.lock().unwrap();
    assert!(
        !recorded
            .iter()
            .any(|c| matches!(c, MockCall::CopyInto { .. })),
        "copy_into should not run when host validation fails: {:?}",
        *recorded
    );
}
