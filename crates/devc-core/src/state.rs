//! Container state management
//!
//! Persists container state to `~/.local/share/devc/containers.json`

use crate::Result;
use chrono::{DateTime, Utc};
use devc_config::GlobalConfig;
use devc_provider::ProviderType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Container state stored on disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    /// Unique identifier (UUID)
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Provider type (docker/podman)
    pub provider: ProviderType,
    /// Path to the devcontainer.json config
    pub config_path: PathBuf,
    /// Docker/Podman image ID (after build)
    pub image_id: Option<String>,
    /// Docker/Podman container ID (after create)
    pub container_id: Option<String>,
    /// Current status
    pub status: DevcContainerStatus,
    /// When this container was created in devc
    pub created_at: DateTime<Utc>,
    /// Last time the container was used
    pub last_used: DateTime<Utc>,
    /// Workspace folder path on host
    pub workspace_path: PathBuf,
    /// Additional metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// Docker Compose project name (if this container uses compose)
    #[serde(default)]
    pub compose_project: Option<String>,
    /// Docker Compose service name (if this container uses compose)
    #[serde(default)]
    pub compose_service: Option<String>,
}

/// devc container status (separate from Docker status)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DevcContainerStatus {
    /// Configuration loaded but not built
    Configured,
    /// Image is being built
    Building,
    /// Image built, container not created
    Built,
    /// Container created but not started
    Created,
    /// Container is running
    Running,
    /// Container stopped
    Stopped,
    /// Container failed (build or runtime error)
    Failed,
}

impl std::fmt::Display for DevcContainerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configured => write!(f, "configured"),
            Self::Building => write!(f, "building"),
            Self::Built => write!(f, "built"),
            Self::Created => write!(f, "created"),
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// State store for all managed containers
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateStore {
    /// Version for forward compatibility
    pub version: u32,
    /// All managed containers indexed by ID
    pub containers: HashMap<String, ContainerState>,
}

impl StateStore {
    const CURRENT_VERSION: u32 = 1;

    /// Create a new empty state store
    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            containers: HashMap::new(),
        }
    }

    /// Load state from the default location
    pub fn load() -> Result<Self> {
        let path = Self::state_path()?;
        Self::load_from(&path)
    }

    /// Load state from a specific path
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let content = std::fs::read_to_string(path)?;
        let store: Self = serde_json::from_str(&content)?;

        // TODO: Handle version migrations if needed
        if store.version > Self::CURRENT_VERSION {
            tracing::warn!(
                "State file version {} is newer than supported version {}",
                store.version,
                Self::CURRENT_VERSION
            );
        }

        Ok(store)
    }

    /// Save state to the default location
    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()?;
        self.save_to(&path)
    }

    /// Save state to a specific path
    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;

        Ok(())
    }

    /// Get the default state file path
    pub fn state_path() -> Result<PathBuf> {
        let data_dir = GlobalConfig::data_dir()?;
        Ok(data_dir.join("containers.json"))
    }

    /// Add a new container state
    pub fn add(&mut self, state: ContainerState) {
        self.containers.insert(state.id.clone(), state);
    }

    /// Get a container state by ID
    pub fn get(&self, id: &str) -> Option<&ContainerState> {
        self.containers.get(id)
    }

    /// Get a mutable container state by ID
    pub fn get_mut(&mut self, id: &str) -> Option<&mut ContainerState> {
        self.containers.get_mut(id)
    }

    /// Find a container by name
    pub fn find_by_name(&self, name: &str) -> Option<&ContainerState> {
        self.containers.values().find(|c| c.name == name)
    }

    /// Find a container by workspace path
    pub fn find_by_workspace(&self, path: &PathBuf) -> Option<&ContainerState> {
        self.containers.values().find(|c| &c.workspace_path == path)
    }

    /// Find a container by config path
    pub fn find_by_config_path(&self, config_path: &Path) -> Option<&ContainerState> {
        self.containers.values().find(|c| c.config_path == config_path)
    }

    /// Remove a container state
    pub fn remove(&mut self, id: &str) -> Option<ContainerState> {
        self.containers.remove(id)
    }

    /// List all containers
    pub fn list(&self) -> Vec<&ContainerState> {
        self.containers.values().collect()
    }

    /// List containers matching a filter
    pub fn filter<F>(&self, f: F) -> Vec<&ContainerState>
    where
        F: Fn(&ContainerState) -> bool,
    {
        self.containers.values().filter(|c| f(c)).collect()
    }

    /// Update last_used timestamp for a container
    pub fn touch(&mut self, id: &str) {
        if let Some(state) = self.containers.get_mut(id) {
            state.last_used = Utc::now();
        }
    }
}

impl ContainerState {
    /// Create a new container state
    pub fn new(
        name: String,
        provider: ProviderType,
        config_path: PathBuf,
        workspace_path: PathBuf,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            provider,
            config_path,
            image_id: None,
            container_id: None,
            status: DevcContainerStatus::Configured,
            created_at: now,
            last_used: now,
            workspace_path,
            metadata: HashMap::new(),
            compose_project: None,
            compose_service: None,
        }
    }

    /// Check if the container can be started
    pub fn can_start(&self) -> bool {
        matches!(
            self.status,
            DevcContainerStatus::Created | DevcContainerStatus::Stopped
        )
    }

    /// Check if the container can be stopped
    pub fn can_stop(&self) -> bool {
        matches!(self.status, DevcContainerStatus::Running)
    }

    /// Check if the container can be removed
    pub fn can_remove(&self) -> bool {
        !matches!(
            self.status,
            DevcContainerStatus::Running | DevcContainerStatus::Building
        )
    }

    /// Get a short display ID
    pub fn short_id(&self) -> &str {
        if self.id.len() > 8 {
            &self.id[..8]
        } else {
            &self.id
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_state(name: &str, status: DevcContainerStatus) -> ContainerState {
        let mut cs = ContainerState::new(
            name.to_string(),
            ProviderType::Docker,
            PathBuf::from("/path/to/devcontainer.json"),
            PathBuf::from(format!("/path/to/{}", name)),
        );
        cs.status = status;
        cs
    }

    #[test]
    fn test_container_state_new() {
        let state = ContainerState::new(
            "test".to_string(),
            ProviderType::Docker,
            PathBuf::from("/path/to/devcontainer.json"),
            PathBuf::from("/path/to/workspace"),
        );

        assert_eq!(state.name, "test");
        assert_eq!(state.provider, ProviderType::Docker);
        assert_eq!(state.status, DevcContainerStatus::Configured);
        assert!(state.image_id.is_none());
        assert!(state.container_id.is_none());
    }

    #[test]
    fn test_state_store_crud() {
        let mut store = StateStore::new();

        let state = ContainerState::new(
            "test".to_string(),
            ProviderType::Docker,
            PathBuf::from("/path/to/devcontainer.json"),
            PathBuf::from("/path/to/workspace"),
        );
        let id = state.id.clone();

        store.add(state);
        assert!(store.get(&id).is_some());
        assert!(store.find_by_name("test").is_some());

        store.remove(&id);
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn test_save_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");

        let mut store = StateStore::new();
        let cs = make_state("roundtrip", DevcContainerStatus::Running);
        let id = cs.id.clone();
        store.add(cs);
        store.save_to(&path).unwrap();

        let loaded = StateStore::load_from(&path).unwrap();
        assert!(loaded.get(&id).is_some());
        assert_eq!(loaded.get(&id).unwrap().name, "roundtrip");
        assert_eq!(loaded.get(&id).unwrap().status, DevcContainerStatus::Running);
    }

    #[test]
    fn test_load_nonexistent_returns_empty() {
        let path = PathBuf::from("/tmp/nonexistent_devc_state_test.json");
        let store = StateStore::load_from(&path).unwrap();
        assert!(store.containers.is_empty());
        assert_eq!(store.version, StateStore::CURRENT_VERSION);
    }

    #[test]
    fn test_load_corrupted_json_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(&path, "not valid json {{{").unwrap();

        let result = StateStore::load_from(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_future_version_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("future.json");
        std::fs::write(
            &path,
            r#"{"version": 999, "containers": {}}"#,
        )
        .unwrap();

        let store = StateStore::load_from(&path).unwrap();
        assert_eq!(store.version, 999);
        assert!(store.containers.is_empty());
    }

    #[test]
    fn test_find_by_workspace() {
        let mut store = StateStore::new();
        let cs = make_state("proj1", DevcContainerStatus::Running);
        store.add(cs);

        let found = store.find_by_workspace(&PathBuf::from("/path/to/proj1"));
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "proj1");

        let not_found = store.find_by_workspace(&PathBuf::from("/path/to/other"));
        assert!(not_found.is_none());
    }

    #[test]
    fn test_filter() {
        let mut store = StateStore::new();
        store.add(make_state("running1", DevcContainerStatus::Running));
        store.add(make_state("stopped1", DevcContainerStatus::Stopped));
        store.add(make_state("running2", DevcContainerStatus::Running));

        let running = store.filter(|c| c.status == DevcContainerStatus::Running);
        assert_eq!(running.len(), 2);

        let stopped = store.filter(|c| c.status == DevcContainerStatus::Stopped);
        assert_eq!(stopped.len(), 1);
    }

    #[test]
    fn test_touch_updates_last_used() {
        let mut store = StateStore::new();
        let cs = make_state("touched", DevcContainerStatus::Running);
        let id = cs.id.clone();
        let original_last_used = cs.last_used;
        store.add(cs);

        // Sleep a tiny bit to ensure time moves forward
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.touch(&id);

        let updated = store.get(&id).unwrap();
        assert!(updated.last_used > original_last_used);
    }

    #[test]
    fn test_can_start_states() {
        assert!(make_state("a", DevcContainerStatus::Created).can_start());
        assert!(make_state("b", DevcContainerStatus::Stopped).can_start());
        assert!(!make_state("c", DevcContainerStatus::Running).can_start());
        assert!(!make_state("d", DevcContainerStatus::Building).can_start());
        assert!(!make_state("e", DevcContainerStatus::Configured).can_start());
    }

    #[test]
    fn test_can_stop_states() {
        assert!(make_state("a", DevcContainerStatus::Running).can_stop());
        assert!(!make_state("b", DevcContainerStatus::Stopped).can_stop());
        assert!(!make_state("c", DevcContainerStatus::Created).can_stop());
    }

    #[test]
    fn test_can_remove_states() {
        assert!(make_state("a", DevcContainerStatus::Stopped).can_remove());
        assert!(make_state("b", DevcContainerStatus::Configured).can_remove());
        assert!(make_state("c", DevcContainerStatus::Built).can_remove());
        assert!(make_state("d", DevcContainerStatus::Failed).can_remove());
        assert!(!make_state("e", DevcContainerStatus::Running).can_remove());
        assert!(!make_state("f", DevcContainerStatus::Building).can_remove());
    }

    #[test]
    fn test_short_id() {
        let cs = make_state("short", DevcContainerStatus::Configured);
        let short = cs.short_id();
        assert_eq!(short.len(), 8);
        assert_eq!(short, &cs.id[..8]);
    }

    #[test]
    fn test_find_by_config_path() {
        let mut store = StateStore::new();
        let mut cs1 = make_state("proj1", DevcContainerStatus::Configured);
        cs1.config_path = PathBuf::from("/workspace/.devcontainer/devcontainer.json");
        let mut cs2 = make_state("proj2", DevcContainerStatus::Configured);
        cs2.config_path = PathBuf::from("/workspace/.devcontainer/python/devcontainer.json");
        store.add(cs1);
        store.add(cs2);

        let found = store.find_by_config_path(Path::new("/workspace/.devcontainer/python/devcontainer.json"));
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "proj2");

        let not_found = store.find_by_config_path(Path::new("/other/devcontainer.json"));
        assert!(not_found.is_none());
    }
}
