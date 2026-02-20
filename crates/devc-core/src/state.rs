//! Container state management
//!
//! Persists container state to `~/.local/share/devc/containers.json`

use crate::Result;
use chrono::{DateTime, Utc};
use devc_config::GlobalConfig;
use devc_provider::{DevcontainerSource, ProviderType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Write content to a file atomically using a temp-file-then-rename pattern.
///
/// Writes to a temporary file in the same directory, then renames it to the
/// target path. This ensures the file is never partially written — a crash
/// during write leaves the old file intact.
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Acquire an exclusive process-wide lock for a state file path.
///
/// The lock lives in a sibling `*.lock` file and is released when the closure
/// returns (or if the process exits).
pub(crate) fn with_path_lock<T, F>(path: &Path, f: F) -> std::io::Result<T>
where
    F: FnOnce() -> std::io::Result<T>,
{
    let _lock = acquire_lock(path)?;
    f()
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}

struct PathLockGuard {
    lock_path: PathBuf,
}

impl Drop for PathLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

fn acquire_lock(path: &Path) -> std::io::Result<PathLockGuard> {
    let lock_path = lock_path_for(path);
    for _ in 0..200 {
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(_) => {
                return Ok(PathLockGuard { lock_path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("timed out waiting for lock {}", lock_path.display()),
    ))
}

/// Merge an in-memory snapshot into latest on-disk state under a lock and save.
///
/// `removed_ids` are treated as tombstones and are removed from the merged result.
pub(crate) fn merge_and_save_snapshot(
    path: &Path,
    snapshot: &StateStore,
    removed_ids: &std::collections::HashSet<String>,
) -> Result<StateStore> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let merged = with_path_lock(path, || {
        let mut disk = StateStore::load_from(path).unwrap_or_else(|_| StateStore::new());
        for id in removed_ids {
            disk.containers.remove(id);
        }
        for (id, cs) in &snapshot.containers {
            if !removed_ids.contains(id) {
                disk.containers.insert(id.clone(), cs.clone());
            }
        }

        let content = serde_json::to_string_pretty(&disk).map_err(std::io::Error::other)?;
        atomic_write(path, content.as_bytes())?;
        Ok(disk)
    })?;
    Ok(merged)
}

fn fnv1a64(input: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET_BASIS;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn short_hash(input: &str, hex_len: usize) -> String {
    format!("{:016x}", fnv1a64(input))
        .chars()
        .take(hex_len)
        .collect()
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
}

fn resolve_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if !dot_git.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(dot_git).ok()?;
    let gitdir_raw = content
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))
        .map(str::trim)?;
    if gitdir_raw.is_empty() {
        return None;
    }

    let gitdir_path = Path::new(gitdir_raw);
    let resolved = if gitdir_path.is_absolute() {
        gitdir_path.to_path_buf()
    } else {
        repo_root.join(gitdir_path)
    };

    resolved.exists().then_some(resolved)
}

fn read_head_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head_line = head.lines().next()?.trim();
    let branch = head_line.strip_prefix("ref: refs/heads/")?.trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

fn read_git_branch(workspace: &Path) -> Option<String> {
    let repo_root = find_git_root(workspace)?;
    let git_dir = resolve_git_dir(&repo_root)?;
    read_head_branch(&git_dir)
}

/// Compute disambiguated human-readable names keyed by container id.
///
/// Rules:
/// - Unique base names keep the original name.
/// - Duplicate names prefer `name (branch)` when branch is known and unique.
/// - Remaining duplicates use `name [hash]` where hash is from config/workspace.
pub fn display_name_map(containers: &[ContainerState]) -> HashMap<String, String> {
    let mut by_name: HashMap<&str, Vec<&ContainerState>> = HashMap::new();
    for c in containers {
        by_name.entry(c.name.as_str()).or_default().push(c);
    }

    let mut out = HashMap::new();

    for (name, group) in by_name {
        if group.len() == 1 {
            let c = group[0];
            out.insert(c.id.clone(), c.name.clone());
            continue;
        }

        let mut branch_counts: HashMap<String, usize> = HashMap::new();
        let mut branch_by_id: HashMap<String, String> = HashMap::new();
        for c in &group {
            if let Some(branch) = c
                .metadata
                .get("git_branch")
                .cloned()
                .or_else(|| c.metadata.get("branch").cloned())
                .or_else(|| read_git_branch(&c.workspace_path))
            {
                *branch_counts.entry(branch.clone()).or_insert(0) += 1;
                branch_by_id.insert(c.id.clone(), branch);
            }
        }

        for c in group {
            if let Some(branch) = branch_by_id.get(&c.id) {
                if branch_counts.get(branch).copied().unwrap_or(0) == 1 {
                    out.insert(c.id.clone(), format!("{} ({})", name, branch));
                    continue;
                }
            }

            let seed = format!(
                "{}::{}",
                c.config_path.to_string_lossy(),
                c.workspace_path.to_string_lossy()
            );
            let hash = short_hash(&seed, 8);
            out.insert(c.id.clone(), format!("{} [{}]", name, hash));
        }
    }

    out
}

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
    /// Source/creator of this container
    #[serde(default = "default_devc_source")]
    pub source: DevcontainerSource,
}

fn default_devc_source() -> DevcontainerSource {
    DevcontainerSource::Devc
}

/// devc container status (separate from Docker status)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DevcContainerStatus {
    /// Config found on disk but not registered — ephemeral, never persisted
    Available,
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
            Self::Available => write!(f, "available"),
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

impl DevcContainerStatus {
    /// Whether this is an ephemeral Available entry (not registered)
    pub fn is_available(self) -> bool {
        matches!(self, Self::Available)
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
    pub fn load_from(path: &Path) -> Result<Self> {
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
    pub fn save_to(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        atomic_write(path, content.as_bytes())?;

        Ok(())
    }

    /// Serialize state to JSON without writing to disk.
    /// Use this to snapshot state under a lock, then write after releasing.
    pub fn serialize(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
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
    pub fn find_by_workspace(&self, path: &Path) -> Option<&ContainerState> {
        self.containers.values().find(|c| c.workspace_path == path)
    }

    /// Find a container by config path
    pub fn find_by_config_path(&self, config_path: &Path) -> Option<&ContainerState> {
        self.containers
            .values()
            .find(|c| c.config_path == config_path)
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
            source: DevcontainerSource::Devc,
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
            DevcContainerStatus::Available
                | DevcContainerStatus::Running
                | DevcContainerStatus::Building
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

    // ==================== atomic_write tests ====================

    #[test]
    fn test_save_to_atomic_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");

        let mut store = StateStore::new();
        store.add(make_state("atomic-test", DevcContainerStatus::Running));
        store.save_to(&path).unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: StateStore = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.containers.len(), 1);
    }

    #[test]
    fn test_save_to_atomic_no_temp_file_left() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");

        let store = StateStore::new();
        store.save_to(&path).unwrap();

        // Check that no .tmp files remain in the directory
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        for entry in &entries {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(!name.contains(".tmp"), "Temp file left behind: {}", name);
        }
    }

    #[test]
    fn test_save_to_atomic_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");

        let mut store1 = StateStore::new();
        store1.add(make_state("first", DevcContainerStatus::Running));
        store1.save_to(&path).unwrap();

        let mut store2 = StateStore::new();
        store2.add(make_state("second", DevcContainerStatus::Stopped));
        store2.save_to(&path).unwrap();

        let loaded = StateStore::load_from(&path).unwrap();
        assert_eq!(loaded.containers.len(), 1);
        assert!(loaded.find_by_name("second").is_some());
        assert!(loaded.find_by_name("first").is_none());
    }

    // ==================== ContainerState tests ====================

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
        assert_eq!(
            loaded.get(&id).unwrap().status,
            DevcContainerStatus::Running
        );
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
        std::fs::write(&path, r#"{"version": 999, "containers": {}}"#).unwrap();

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
        assert!(!make_state("g", DevcContainerStatus::Available).can_remove());
    }

    #[test]
    fn test_available_status() {
        let cs = make_state("avail", DevcContainerStatus::Available);
        assert!(cs.status.is_available());
        assert!(!cs.can_start());
        assert!(!cs.can_stop());
        assert!(!cs.can_remove());
        assert_eq!(cs.status.to_string(), "available");
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

        let found = store.find_by_config_path(Path::new(
            "/workspace/.devcontainer/python/devcontainer.json",
        ));
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "proj2");

        let not_found = store.find_by_config_path(Path::new("/other/devcontainer.json"));
        assert!(not_found.is_none());
    }

    #[test]
    fn test_display_name_map_unique_name_unchanged() {
        let cs = make_state("proj", DevcContainerStatus::Running);
        let map = display_name_map(std::slice::from_ref(&cs));
        assert_eq!(map.get(&cs.id).cloned(), Some("proj".to_string()));
    }

    #[test]
    fn test_display_name_map_duplicate_names_use_hash() {
        let mut a = make_state("proj", DevcContainerStatus::Running);
        a.workspace_path = PathBuf::from("/tmp/branch-a/proj");
        a.config_path = PathBuf::from("/tmp/branch-a/proj/.devcontainer/devcontainer.json");

        let mut b = make_state("proj", DevcContainerStatus::Running);
        b.workspace_path = PathBuf::from("/tmp/branch-b/proj");
        b.config_path = PathBuf::from("/tmp/branch-b/proj/.devcontainer/devcontainer.json");

        let map = display_name_map(&[a.clone(), b.clone()]);
        let an = map.get(&a.id).unwrap();
        let bn = map.get(&b.id).unwrap();
        assert_ne!(an, bn);
        assert!(an.starts_with("proj ["));
        assert!(bn.starts_with("proj ["));
    }

    #[test]
    fn test_display_name_map_duplicate_names_use_unique_branch() {
        let mut a = make_state("proj", DevcContainerStatus::Running);
        a.metadata
            .insert("git_branch".to_string(), "feature-a".to_string());

        let mut b = make_state("proj", DevcContainerStatus::Running);
        b.metadata
            .insert("git_branch".to_string(), "feature-b".to_string());

        let map = display_name_map(&[a.clone(), b.clone()]);
        assert_eq!(
            map.get(&a.id).cloned(),
            Some("proj (feature-a)".to_string())
        );
        assert_eq!(
            map.get(&b.id).cloned(),
            Some("proj (feature-b)".to_string())
        );
    }

    #[test]
    fn test_read_git_branch_from_dot_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let ws = repo.join("subdir");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        assert_eq!(read_git_branch(&ws), Some("main".to_string()));
    }

    #[test]
    fn test_read_git_branch_from_gitdir_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let ws = repo.join("subdir");
        let actual_git = tmp.path().join("repo.git");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&actual_git).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: ../repo.git\n").unwrap();
        std::fs::write(
            actual_git.join("HEAD"),
            "ref: refs/heads/feature/worktree\n",
        )
        .unwrap();

        assert_eq!(read_git_branch(&ws), Some("feature/worktree".to_string()));
    }

    #[test]
    fn test_display_name_map_uses_workspace_gitdir_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_a = tmp.path().join("repo-a");
        let repo_b = tmp.path().join("repo-b");
        let git_a = tmp.path().join("repo-a.git");
        let git_b = tmp.path().join("repo-b.git");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        std::fs::create_dir_all(&git_a).unwrap();
        std::fs::create_dir_all(&git_b).unwrap();
        std::fs::write(repo_a.join(".git"), "gitdir: ../repo-a.git\n").unwrap();
        std::fs::write(repo_b.join(".git"), "gitdir: ../repo-b.git\n").unwrap();
        std::fs::write(git_a.join("HEAD"), "ref: refs/heads/alpha\n").unwrap();
        std::fs::write(git_b.join("HEAD"), "ref: refs/heads/beta\n").unwrap();

        let mut a = make_state("proj", DevcContainerStatus::Running);
        a.workspace_path = repo_a.clone();
        a.config_path = repo_a.join(".devcontainer/devcontainer.json");

        let mut b = make_state("proj", DevcContainerStatus::Running);
        b.workspace_path = repo_b.clone();
        b.config_path = repo_b.join(".devcontainer/devcontainer.json");

        let map = display_name_map(&[a.clone(), b.clone()]);
        assert_eq!(map.get(&a.id).cloned(), Some("proj (alpha)".to_string()));
        assert_eq!(map.get(&b.id).cloned(), Some("proj (beta)".to_string()));
    }

    #[test]
    fn test_merge_and_save_snapshot_preserves_disjoint_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("containers.json");

        let mut disk = StateStore::new();
        let mut a = make_state("a", DevcContainerStatus::Configured);
        a.metadata.insert("k".to_string(), "v1".to_string());
        disk.add(a.clone());
        disk.save_to(&path).unwrap();

        let mut snapshot = StateStore::new();
        let mut b = make_state("b", DevcContainerStatus::Running);
        b.metadata.insert("x".to_string(), "y".to_string());
        snapshot.add(b.clone());

        let merged =
            merge_and_save_snapshot(&path, &snapshot, &std::collections::HashSet::new()).unwrap();
        assert!(merged.find_by_name("a").is_some());
        assert!(merged.find_by_name("b").is_some());
    }

    #[test]
    fn test_merge_and_save_snapshot_applies_tombstones() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("containers.json");

        let mut disk = StateStore::new();
        let a = make_state("a", DevcContainerStatus::Configured);
        let a_id = a.id.clone();
        disk.add(a);
        disk.save_to(&path).unwrap();

        let snapshot = StateStore::new();
        let mut removed = std::collections::HashSet::new();
        removed.insert(a_id);

        let merged = merge_and_save_snapshot(&path, &snapshot, &removed).unwrap();
        assert!(merged.containers.is_empty());
    }
}
