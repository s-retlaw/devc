//! Feature reference parsing, option conversion, and ordering

use crate::CoreError;
use devc_config::{Command, FeatureConfig, Mount};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Where a feature comes from
#[derive(Debug, Clone)]
pub enum FeatureSource {
    /// OCI registry artifact
    Oci {
        registry: String,
        namespace: String,
        name: String,
        tag: String,
    },
    /// Local directory path
    Local { path: PathBuf },
    /// HTTP/HTTPS tarball URL
    TarballUrl { url: String },
}

/// Metadata from devcontainer-feature.json inside a feature tarball
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct FeatureMetadata {
    pub id: Option<String>,
    pub version: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(alias = "installsAfter")]
    pub install_after: Option<Vec<String>>,
    /// Option definitions with default values
    pub options: Option<HashMap<String, FeatureOptionDef>>,
    /// Environment variables to set in the container
    pub container_env: Option<HashMap<String, String>>,
    /// Environment variables for tool/exec processes (not container creation)
    pub remote_env: Option<HashMap<String, String>>,
    /// Linux capabilities to add (e.g. SYS_PTRACE)
    pub cap_add: Option<Vec<String>>,
    /// Security options (e.g. seccomp=unconfined)
    pub security_opt: Option<Vec<String>>,
    /// Whether to run an init process inside the container
    pub init: Option<bool>,
    /// Whether to run the container in privileged mode
    pub privileged: Option<bool>,
    /// Mounts to add to the container
    pub mounts: Option<Vec<Mount>>,
    /// Command to run when the container is first created
    pub on_create_command: Option<Command>,
    /// Command to run after the container is created
    pub post_create_command: Option<Command>,
    /// Command to run after the container starts
    pub post_start_command: Option<Command>,
    /// Command to run after attaching to the container
    pub post_attach_command: Option<Command>,
    /// Command to run when content is updated (between onCreate and postCreate)
    pub update_content_command: Option<Command>,
    /// Entrypoint to use for the container (e.g. dockerd-entrypoint.sh for docker-in-docker)
    pub entrypoint: Option<String>,
    /// Hard dependencies on other features (feature ID → config).
    /// Transitive deps are automatically resolved and pulled in.
    pub depends_on: Option<HashMap<String, serde_json::Value>>,
}

/// A single option definition from devcontainer-feature.json
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FeatureOptionDef {
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

/// Container-level properties merged from all resolved features.
///
/// Arrays are unioned (deduplicated), booleans are OR'd.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MergedFeatureProperties {
    #[serde(default)]
    pub cap_add: Vec<String>,
    #[serde(default)]
    pub security_opt: Vec<String>,
    #[serde(default)]
    pub init: bool,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub mounts: Vec<Mount>,
    /// Lifecycle commands from features (ordered by feature install order).
    /// Per spec, feature lifecycle commands run BEFORE devcontainer.json commands.
    #[serde(default)]
    pub on_create_commands: Vec<Command>,
    #[serde(default)]
    pub post_create_commands: Vec<Command>,
    #[serde(default)]
    pub post_start_commands: Vec<Command>,
    #[serde(default)]
    pub post_attach_commands: Vec<Command>,
    /// remoteEnv from features — applied to exec/lifecycle commands, not container creation
    #[serde(default)]
    pub remote_env: HashMap<String, String>,
    /// Lifecycle: updateContentCommand(s) from features (ordered by feature install order)
    #[serde(default)]
    pub update_content_commands: Vec<Command>,
    /// Entrypoint override from features (last feature wins — only one entrypoint can be active)
    #[serde(default)]
    pub entrypoint: Option<String>,
}

impl MergedFeatureProperties {
    /// Returns true if any container-level properties (capAdd, securityOpt, init, privileged)
    /// need to be applied. Mounts and lifecycle commands are not considered here.
    pub fn has_container_properties(&self) -> bool {
        !self.cap_add.is_empty()
            || !self.security_opt.is_empty()
            || self.init
            || self.privileged
            || self.entrypoint.is_some()
    }

    /// Returns `Some(&remote_env)` if non-empty, `None` otherwise.
    /// Used to pass feature remote env to exec/shell config builders.
    pub fn remote_env_option(&self) -> Option<&HashMap<String, String>> {
        if self.remote_env.is_empty() {
            None
        } else {
            Some(&self.remote_env)
        }
    }
}

/// Merge container-level properties from all resolved features.
///
/// - `cap_add` and `security_opt` are unioned across features (deduplicated).
/// - `init` and `privileged` are OR'd (true if any feature requests them).
pub fn merge_feature_properties(features: &[ResolvedFeature]) -> MergedFeatureProperties {
    let mut result = MergedFeatureProperties::default();

    for feature in features {
        if let Some(ref caps) = feature.metadata.cap_add {
            for cap in caps {
                if !result.cap_add.contains(cap) {
                    result.cap_add.push(cap.clone());
                }
            }
        }
        if let Some(ref opts) = feature.metadata.security_opt {
            for opt in opts {
                if !result.security_opt.contains(opt) {
                    result.security_opt.push(opt.clone());
                }
            }
        }
        if feature.metadata.init == Some(true) {
            result.init = true;
        }
        if feature.metadata.privileged == Some(true) {
            result.privileged = true;
        }
        if let Some(ref mounts) = feature.metadata.mounts {
            for mount in mounts {
                if !result.mounts.contains(mount) {
                    result.mounts.push(mount.clone());
                }
            }
        }
        if let Some(ref cmd) = feature.metadata.on_create_command {
            result.on_create_commands.push(cmd.clone());
        }
        if let Some(ref cmd) = feature.metadata.post_create_command {
            result.post_create_commands.push(cmd.clone());
        }
        if let Some(ref cmd) = feature.metadata.post_start_command {
            result.post_start_commands.push(cmd.clone());
        }
        if let Some(ref cmd) = feature.metadata.post_attach_command {
            result.post_attach_commands.push(cmd.clone());
        }
        if let Some(ref env) = feature.metadata.remote_env {
            for (key, value) in env {
                result.remote_env.insert(key.clone(), value.clone());
            }
        }
        if let Some(ref cmd) = feature.metadata.update_content_command {
            result.update_content_commands.push(cmd.clone());
        }
        // Entrypoint: last feature wins (only one entrypoint can be active)
        if let Some(ref ep) = feature.metadata.entrypoint {
            result.entrypoint = Some(ep.clone());
        }
    }

    result
}

/// Merge user-provided options with feature metadata defaults.
///
/// For any option defined in metadata that the user didn't specify,
/// the default value from the metadata is used.
pub fn merge_options_with_defaults(
    user_options: &HashMap<String, String>,
    metadata: &FeatureMetadata,
) -> HashMap<String, String> {
    let mut merged = HashMap::new();

    // Start with defaults from metadata
    if let Some(ref option_defs) = metadata.options {
        for (key, def) in option_defs {
            if let Some(ref default_val) = def.default {
                let val = match default_val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    other => other.to_string(),
                };
                merged.insert(key.clone(), val);
            }
        }
    }

    // Override with user-provided options
    for (key, val) in user_options {
        merged.insert(key.clone(), val.clone());
    }

    merged
}

/// A fully resolved feature ready for Dockerfile generation
#[derive(Debug, Clone)]
pub struct ResolvedFeature {
    /// The original feature ID string (e.g. "ghcr.io/devcontainers/features/node:1")
    pub id: String,
    /// Directory containing install.sh and devcontainer-feature.json
    pub dir: PathBuf,
    /// Options to pass as environment variables
    pub options: HashMap<String, String>,
    /// Metadata from devcontainer-feature.json
    pub metadata: FeatureMetadata,
}

/// Parse a feature reference string into a FeatureSource.
///
/// Local paths start with `.` or `/`.
/// URLs starting with `http://` or `https://` are treated as tarball URLs.
/// Everything else is treated as an OCI reference.
pub fn parse_feature_ref(id: &str) -> FeatureSource {
    if id.starts_with('.') || id.starts_with('/') {
        FeatureSource::Local {
            path: PathBuf::from(id),
        }
    } else if id.starts_with("https://") || id.starts_with("http://") {
        FeatureSource::TarballUrl {
            url: id.to_string(),
        }
    } else {
        parse_oci_ref(id)
    }
}

/// Parse an OCI feature reference like `ghcr.io/devcontainers/features/node:1`
fn parse_oci_ref(id: &str) -> FeatureSource {
    // Split tag
    let (path, tag) = match id.rsplit_once(':') {
        Some((p, t)) => (p, t.to_string()),
        None => (id, "latest".to_string()),
    };

    // Split into registry and the rest
    // If the first segment contains a dot or colon, it's a registry hostname
    let parts: Vec<&str> = path.splitn(2, '/').collect();
    let (registry, remainder) =
        if parts.len() == 2 && (parts[0].contains('.') || parts[0].contains(':')) {
            (parts[0].to_string(), parts[1])
        } else {
            // Default registry (shouldn't normally happen for devcontainer features)
            ("ghcr.io".to_string(), path)
        };

    // Split remainder into namespace (everything before last /) and name (last segment)
    let (namespace, name) = match remainder.rsplit_once('/') {
        Some((ns, n)) => (ns.to_string(), n.to_string()),
        None => (String::new(), remainder.to_string()),
    };

    FeatureSource::Oci {
        registry,
        namespace,
        name,
        tag,
    }
}

/// Convert a FeatureConfig into a string-string options map.
///
/// Returns None if the feature is disabled (Bool(false)).
pub fn feature_options(config: &FeatureConfig) -> Option<HashMap<String, String>> {
    match config {
        FeatureConfig::Bool(false) => None,
        FeatureConfig::Bool(true) => Some(HashMap::new()),
        FeatureConfig::Version(v) => {
            let mut m = HashMap::new();
            m.insert("version".to_string(), v.clone());
            Some(m)
        }
        FeatureConfig::Options(map) => {
            let m = map
                .iter()
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), val)
                })
                .collect();
            Some(m)
        }
    }
}

/// Convert a `dependsOn` entry value into an options map.
///
/// Returns `None` if the dependency is disabled (`false`).
/// Returns `Some(empty map)` for `true` or `{}`.
/// Returns `Some(map)` for `{"version": "3"}` etc.
pub fn parse_depends_on_value(value: &serde_json::Value) -> Option<HashMap<String, String>> {
    match value {
        serde_json::Value::Bool(false) => None,
        serde_json::Value::Bool(true) => Some(HashMap::new()),
        serde_json::Value::Object(map) => {
            let m = map
                .iter()
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), val)
                })
                .collect();
            Some(m)
        }
        // Treat anything else (null, string, etc.) as enabled with no options
        _ => Some(HashMap::new()),
    }
}

/// Order resolved features respecting `dependsOn` (hard) and `installsAfter` (soft) constraints.
///
/// Hard dependencies (`dependsOn`) must be satisfied — cycles among them produce an error.
/// Soft dependencies (`installsAfter`) are best-effort — cycles are broken by falling back
/// to declaration order.
pub fn order_features(features: Vec<ResolvedFeature>) -> crate::Result<Vec<ResolvedFeature>> {
    if features.len() <= 1 {
        return Ok(features);
    }

    let n = features.len();

    // Build indices: both short-id and full-id map to position
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, f) in features.iter().enumerate() {
        let short_id = extract_feature_short_id(&f.id);
        id_to_idx.insert(short_id, i);
        id_to_idx.insert(f.id.clone(), i);
    }

    // Track which edges are hard (dependsOn) vs soft (installsAfter)
    let mut is_hard_edge: Vec<Vec<bool>> = vec![vec![]; n]; // is_hard_edge[j] parallel to dependents[j]
    let mut after_count = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];

    // Helper to find a feature index by reference id
    let find_idx = |dep_id: &str, self_idx: usize| -> Option<usize> {
        // Try exact match first, then short-id
        if let Some(&j) = id_to_idx.get(dep_id) {
            if j != self_idx {
                return Some(j);
            }
        }
        let short = extract_feature_short_id(dep_id);
        if let Some(&j) = id_to_idx.get(&short) {
            if j != self_idx {
                return Some(j);
            }
        }
        None
    };

    // Build edges from dependsOn (hard)
    for (i, f) in features.iter().enumerate() {
        if let Some(ref deps) = f.metadata.depends_on {
            for dep_id in deps.keys() {
                if let Some(j) = find_idx(dep_id, i) {
                    dependents[j].push(i);
                    is_hard_edge[j].push(true);
                    after_count[i] += 1;
                }
            }
        }
    }

    // Build edges from installsAfter (soft)
    for (i, f) in features.iter().enumerate() {
        if let Some(ref install_after) = f.metadata.install_after {
            for dep_id in install_after {
                if let Some(j) = find_idx(dep_id, i) {
                    // Avoid duplicate edge if already added via dependsOn
                    if !dependents[j].contains(&i) {
                        dependents[j].push(i);
                        is_hard_edge[j].push(false);
                        after_count[i] += 1;
                    }
                }
                // Unknown soft dependencies are silently ignored
            }
        }
    }

    // Kahn's algorithm, using declaration order as tiebreaker
    let mut queue: Vec<usize> = (0..n).filter(|&i| after_count[i] == 0).collect();
    let mut result_indices = Vec::with_capacity(n);

    while let Some(idx) = queue.first().copied() {
        queue.remove(0);
        result_indices.push(idx);

        let mut deps: Vec<(usize, bool)> = dependents[idx]
            .iter()
            .zip(is_hard_edge[idx].iter())
            .map(|(&dep, &hard)| (dep, hard))
            .collect();
        deps.sort_by_key(|&(dep, _)| dep);

        for &(dep, _) in &deps {
            after_count[dep] -= 1;
            if after_count[dep] == 0 {
                let pos = queue.partition_point(|&x| x < dep);
                queue.insert(pos, dep);
            }
        }
    }

    // Handle remaining features (cycles)
    if result_indices.len() < n {
        // Check if any stuck feature has an unsatisfied hard dependency
        let stuck: Vec<usize> = (0..n).filter(|i| !result_indices.contains(i)).collect();

        // Check for hard cycles: does any stuck feature have a hard edge from another stuck feature?
        let stuck_set: std::collections::HashSet<usize> = stuck.iter().copied().collect();
        let mut has_hard_cycle = false;
        let mut cycle_features = Vec::new();

        for &j in &stuck {
            for (idx, &dep) in dependents[j].iter().enumerate() {
                if stuck_set.contains(&dep) && is_hard_edge[j][idx] {
                    has_hard_cycle = true;
                    // Collect feature IDs involved
                    if !cycle_features.contains(&features[j].id) {
                        cycle_features.push(features[j].id.clone());
                    }
                    if !cycle_features.contains(&features[dep].id) {
                        cycle_features.push(features[dep].id.clone());
                    }
                }
            }
        }

        if has_hard_cycle {
            return Err(CoreError::FeatureDependencyCycle(
                cycle_features.join(" -> "),
            ));
        }

        // Only soft cycles — break by appending in declaration order
        for i in stuck {
            result_indices.push(i);
        }
    }

    // Reorder
    let mut features = features;
    let mut ordered = Vec::with_capacity(n);
    let mut slots: Vec<Option<ResolvedFeature>> = features.drain(..).map(Some).collect();
    for idx in result_indices {
        if let Some(f) = slots[idx].take() {
            ordered.push(f);
        }
    }
    Ok(ordered)
}

/// Extract the short feature ID (last path segment, no tag) for matching installsAfter
fn extract_feature_short_id(id: &str) -> String {
    // URL features: strip query string, take last path segment, strip tarball extensions
    if id.starts_with("https://") || id.starts_with("http://") {
        let without_query = id.split('?').next().unwrap_or(id);
        let segment = without_query.rsplit('/').next().unwrap_or(without_query);
        let name = segment
            .strip_suffix(".tar.gz")
            .or_else(|| segment.strip_suffix(".tgz"))
            .or_else(|| segment.strip_suffix(".tar"))
            .unwrap_or(segment);
        return name.to_string();
    }

    // OCI refs: strip tag, take last segment
    let without_tag = match id.rsplit_once(':') {
        Some((p, _)) => p,
        None => id,
    };
    without_tag
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(without_tag)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_oci_ref_full() {
        let source = parse_feature_ref("ghcr.io/devcontainers/features/node:1");
        match source {
            FeatureSource::Oci {
                registry,
                namespace,
                name,
                tag,
            } => {
                assert_eq!(registry, "ghcr.io");
                assert_eq!(namespace, "devcontainers/features");
                assert_eq!(name, "node");
                assert_eq!(tag, "1");
            }
            _ => panic!("Expected OCI source"),
        }
    }

    #[test]
    fn test_parse_oci_ref_no_tag() {
        let source = parse_feature_ref("ghcr.io/devcontainers/features/git");
        match source {
            FeatureSource::Oci { tag, name, .. } => {
                assert_eq!(name, "git");
                assert_eq!(tag, "latest");
            }
            _ => panic!("Expected OCI source"),
        }
    }

    #[test]
    fn test_parse_oci_ref_custom_registry() {
        let source = parse_feature_ref("myregistry.azurecr.io/features/myfeature:2");
        match source {
            FeatureSource::Oci {
                registry,
                namespace,
                name,
                tag,
            } => {
                assert_eq!(registry, "myregistry.azurecr.io");
                assert_eq!(namespace, "features");
                assert_eq!(name, "myfeature");
                assert_eq!(tag, "2");
            }
            _ => panic!("Expected OCI source"),
        }
    }

    #[test]
    fn test_parse_local_path_dot() {
        let source = parse_feature_ref("./my-feature");
        match source {
            FeatureSource::Local { path } => {
                assert_eq!(path, PathBuf::from("./my-feature"));
            }
            _ => panic!("Expected Local source"),
        }
    }

    #[test]
    fn test_parse_local_path_absolute() {
        let source = parse_feature_ref("/home/user/features/custom");
        match source {
            FeatureSource::Local { path } => {
                assert_eq!(path, PathBuf::from("/home/user/features/custom"));
            }
            _ => panic!("Expected Local source"),
        }
    }

    #[test]
    fn test_feature_options_bool_true() {
        let opts = feature_options(&FeatureConfig::Bool(true));
        assert_eq!(opts, Some(HashMap::new()));
    }

    #[test]
    fn test_feature_options_bool_false() {
        let opts = feature_options(&FeatureConfig::Bool(false));
        assert!(opts.is_none());
    }

    #[test]
    fn test_feature_options_version() {
        let opts = feature_options(&FeatureConfig::Version("18".to_string()));
        let mut expected = HashMap::new();
        expected.insert("version".to_string(), "18".to_string());
        assert_eq!(opts, Some(expected));
    }

    #[test]
    fn test_feature_options_map() {
        let mut map = HashMap::new();
        map.insert(
            "version".to_string(),
            serde_json::Value::String("20".to_string()),
        );
        map.insert("nodeGypDependencies".to_string(), serde_json::json!(true));
        let opts = feature_options(&FeatureConfig::Options(map)).unwrap();
        assert_eq!(opts.get("version").unwrap(), "20");
        assert_eq!(opts.get("nodeGypDependencies").unwrap(), "true");
    }

    #[test]
    fn test_order_features_no_deps() {
        let features = vec![
            make_test_feature("a", None),
            make_test_feature("b", None),
            make_test_feature("c", None),
        ];
        let ordered = order_features(features).unwrap();
        let ids: Vec<&str> = ordered.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_order_features_with_install_after() {
        let features = vec![
            make_test_feature(
                "ghcr.io/devcontainers/features/node:1",
                Some(vec!["common-utils".to_string()]),
            ),
            make_test_feature("ghcr.io/devcontainers/features/common-utils:1", None),
        ];
        let ordered = order_features(features).unwrap();
        let ids: Vec<&str> = ordered.iter().map(|f| f.id.as_str()).collect();
        // common-utils should come first because node depends on it
        assert_eq!(
            ids,
            vec![
                "ghcr.io/devcontainers/features/common-utils:1",
                "ghcr.io/devcontainers/features/node:1"
            ]
        );
    }

    #[test]
    fn test_order_features_unknown_dep_ignored() {
        let features = vec![
            make_test_feature("a", Some(vec!["unknown".to_string()])),
            make_test_feature("b", None),
        ];
        let ordered = order_features(features).unwrap();
        let ids: Vec<&str> = ordered.iter().map(|f| f.id.as_str()).collect();
        // Unknown dep is ignored, original order preserved
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn test_extract_feature_short_id() {
        assert_eq!(
            extract_feature_short_id("ghcr.io/devcontainers/features/node:1"),
            "node"
        );
        assert_eq!(
            extract_feature_short_id("ghcr.io/devcontainers/features/common-utils"),
            "common-utils"
        );
        assert_eq!(extract_feature_short_id("node"), "node");

        // URL features — should extract feature name, not "https"
        assert_eq!(
            extract_feature_short_id("https://example.com/my-feature.tar.gz"),
            "my-feature"
        );
        assert_eq!(
            extract_feature_short_id("http://internal:8080/releases/cool-tool.tgz"),
            "cool-tool"
        );
        assert_eq!(
            extract_feature_short_id("https://example.com/feat.tar"),
            "feat"
        );
        assert_eq!(
            extract_feature_short_id("https://example.com/plain-name"),
            "plain-name"
        );
        assert_eq!(
            extract_feature_short_id("https://example.com/feature.tar.gz?token=abc"),
            "feature"
        );
    }

    #[test]
    fn test_merge_options_with_defaults() {
        let metadata = FeatureMetadata {
            options: Some({
                let mut m = HashMap::new();
                m.insert(
                    "version".to_string(),
                    FeatureOptionDef {
                        default: Some(serde_json::Value::String("os-provided".to_string())),
                    },
                );
                m.insert(
                    "ppa".to_string(),
                    FeatureOptionDef {
                        default: Some(serde_json::Value::Bool(true)),
                    },
                );
                m
            }),
            ..Default::default()
        };

        // User provides version but not ppa → ppa gets default
        let mut user_opts = HashMap::new();
        user_opts.insert("version".to_string(), "2.40.0".to_string());
        let merged = merge_options_with_defaults(&user_opts, &metadata);
        assert_eq!(merged.get("version").unwrap(), "2.40.0");
        assert_eq!(merged.get("ppa").unwrap(), "true");

        // User provides nothing → both get defaults
        let merged = merge_options_with_defaults(&HashMap::new(), &metadata);
        assert_eq!(merged.get("version").unwrap(), "os-provided");
        assert_eq!(merged.get("ppa").unwrap(), "true");
    }

    #[test]
    fn test_merge_options_no_metadata_defaults() {
        let metadata = FeatureMetadata::default();
        let mut user_opts = HashMap::new();
        user_opts.insert("version".to_string(), "20".to_string());
        let merged = merge_options_with_defaults(&user_opts, &metadata);
        assert_eq!(merged.get("version").unwrap(), "20");
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn test_parse_feature_ref_https_url() {
        let source = parse_feature_ref("https://example.com/feature.tar.gz");
        match source {
            FeatureSource::TarballUrl { url } => {
                assert_eq!(url, "https://example.com/feature.tar.gz");
            }
            _ => panic!("Expected TarballUrl source"),
        }
    }

    #[test]
    fn test_parse_feature_ref_http_url() {
        let source = parse_feature_ref("http://internal:8080/feat.tgz");
        match source {
            FeatureSource::TarballUrl { url } => {
                assert_eq!(url, "http://internal:8080/feat.tgz");
            }
            _ => panic!("Expected TarballUrl source"),
        }
    }

    #[test]
    fn test_parse_feature_ref_url_not_oci() {
        // URLs should not be misrouted to the OCI parser
        let source =
            parse_feature_ref("https://github.com/user/repo/releases/download/v1/feature.tar.gz");
        match source {
            FeatureSource::TarballUrl { url } => {
                assert!(url.starts_with("https://"));
            }
            _ => panic!("Expected TarballUrl source, not OCI"),
        }
    }

    fn make_test_feature(id: &str, install_after: Option<Vec<String>>) -> ResolvedFeature {
        ResolvedFeature {
            id: id.to_string(),
            dir: PathBuf::from(format!("/tmp/features/{}", id)),
            options: HashMap::new(),
            metadata: FeatureMetadata {
                id: Some(id.to_string()),
                install_after,
                ..Default::default()
            },
        }
    }

    fn make_feature_with_props(
        id: &str,
        cap_add: Option<Vec<String>>,
        security_opt: Option<Vec<String>>,
        init: Option<bool>,
        privileged: Option<bool>,
    ) -> ResolvedFeature {
        ResolvedFeature {
            id: id.to_string(),
            dir: PathBuf::from(format!("/tmp/features/{}", id)),
            options: HashMap::new(),
            metadata: FeatureMetadata {
                id: Some(id.to_string()),
                cap_add,
                security_opt,
                init,
                privileged,
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_merge_feature_properties_empty() {
        let result = merge_feature_properties(&[]);
        assert_eq!(result, MergedFeatureProperties::default());
    }

    #[test]
    fn test_merge_feature_properties_no_props() {
        let features = vec![make_test_feature("a", None), make_test_feature("b", None)];
        let result = merge_feature_properties(&features);
        assert_eq!(result, MergedFeatureProperties::default());
    }

    #[test]
    fn test_merge_feature_properties_single_cap_add() {
        let features = vec![make_feature_with_props(
            "go",
            Some(vec!["SYS_PTRACE".to_string()]),
            Some(vec!["seccomp=unconfined".to_string()]),
            None,
            None,
        )];
        let result = merge_feature_properties(&features);
        assert_eq!(result.cap_add, vec!["SYS_PTRACE"]);
        assert_eq!(result.security_opt, vec!["seccomp=unconfined"]);
        assert!(!result.init);
        assert!(!result.privileged);
    }

    #[test]
    fn test_merge_feature_properties_multiple_overlapping() {
        let features = vec![
            make_feature_with_props(
                "go",
                Some(vec!["SYS_PTRACE".to_string()]),
                Some(vec!["seccomp=unconfined".to_string()]),
                None,
                None,
            ),
            make_feature_with_props(
                "cpp",
                Some(vec!["SYS_PTRACE".to_string(), "NET_ADMIN".to_string()]),
                Some(vec![
                    "seccomp=unconfined".to_string(),
                    "apparmor=unconfined".to_string(),
                ]),
                Some(true),
                None,
            ),
        ];
        let result = merge_feature_properties(&features);
        // SYS_PTRACE should appear once (deduplicated)
        assert_eq!(result.cap_add, vec!["SYS_PTRACE", "NET_ADMIN"]);
        assert_eq!(
            result.security_opt,
            vec!["seccomp=unconfined", "apparmor=unconfined"]
        );
        assert!(result.init);
        assert!(!result.privileged);
    }

    #[test]
    fn test_merge_feature_properties_boolean_or() {
        let features = vec![
            make_feature_with_props("a", None, None, Some(false), Some(false)),
            make_feature_with_props("b", None, None, Some(true), Some(false)),
            make_feature_with_props("c", None, None, Some(false), Some(true)),
        ];
        let result = merge_feature_properties(&features);
        assert!(result.init, "init should be true (any feature sets it)");
        assert!(
            result.privileged,
            "privileged should be true (any feature sets it)"
        );
    }

    #[test]
    fn test_merged_feature_properties_serialization() {
        let props = MergedFeatureProperties {
            cap_add: vec!["SYS_PTRACE".to_string()],
            security_opt: vec!["seccomp=unconfined".to_string()],
            init: true,
            privileged: false,
            mounts: vec![Mount::String(
                "type=volume,source=myvolume,target=/data".to_string(),
            )],
            ..Default::default()
        };
        let json = serde_json::to_string(&props).unwrap();
        let deserialized: MergedFeatureProperties = serde_json::from_str(&json).unwrap();
        assert_eq!(props, deserialized);
    }

    #[test]
    fn test_feature_metadata_deserialize_container_props() {
        let json = r#"{
            "id": "go",
            "capAdd": ["SYS_PTRACE"],
            "securityOpt": ["seccomp=unconfined"],
            "init": true,
            "privileged": false
        }"#;
        let metadata: FeatureMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(metadata.cap_add, Some(vec!["SYS_PTRACE".to_string()]));
        assert_eq!(
            metadata.security_opt,
            Some(vec!["seccomp=unconfined".to_string()])
        );
        assert_eq!(metadata.init, Some(true));
        assert_eq!(metadata.privileged, Some(false));
    }

    #[test]
    fn test_merge_feature_properties_with_mounts() {
        let features = vec![
            ResolvedFeature {
                id: "feat-a".to_string(),
                dir: PathBuf::from("/tmp/features/a"),
                options: HashMap::new(),
                metadata: FeatureMetadata {
                    mounts: Some(vec![Mount::String(
                        "type=volume,source=vol-a,target=/data-a".to_string(),
                    )]),
                    ..Default::default()
                },
            },
            ResolvedFeature {
                id: "feat-b".to_string(),
                dir: PathBuf::from("/tmp/features/b"),
                options: HashMap::new(),
                metadata: FeatureMetadata {
                    mounts: Some(vec![
                        Mount::String("type=volume,source=vol-b,target=/data-b".to_string()),
                        // Duplicate of feat-a mount — should be deduplicated
                        Mount::String("type=volume,source=vol-a,target=/data-a".to_string()),
                    ]),
                    ..Default::default()
                },
            },
        ];
        let result = merge_feature_properties(&features);
        assert_eq!(
            result.mounts.len(),
            2,
            "duplicate mount should be deduplicated"
        );
        assert_eq!(
            result.mounts[0],
            Mount::String("type=volume,source=vol-a,target=/data-a".to_string())
        );
        assert_eq!(
            result.mounts[1],
            Mount::String("type=volume,source=vol-b,target=/data-b".to_string())
        );
    }

    #[test]
    fn test_merge_feature_properties_with_object_mounts() {
        use devc_config::MountObject;

        let features = vec![ResolvedFeature {
            id: "feat".to_string(),
            dir: PathBuf::from("/tmp/features/feat"),
            options: HashMap::new(),
            metadata: FeatureMetadata {
                mounts: Some(vec![Mount::Object(MountObject {
                    mount_type: Some("volume".to_string()),
                    source: Some("my-vol".to_string()),
                    target: "/workspace/data".to_string(),
                    read_only: Some(false),
                })]),
                ..Default::default()
            },
        }];
        let result = merge_feature_properties(&features);
        assert_eq!(result.mounts.len(), 1);
        match &result.mounts[0] {
            Mount::Object(obj) => {
                assert_eq!(obj.mount_type.as_deref(), Some("volume"));
                assert_eq!(obj.target, "/workspace/data");
            }
            _ => panic!("Expected object mount"),
        }
    }

    #[test]
    fn test_feature_metadata_deserialize_with_mounts() {
        let json = r#"{
            "id": "my-feature",
            "mounts": [
                "type=volume,source=my-vol,target=/data",
                {
                    "type": "tmpfs",
                    "target": "/tmp/scratch"
                }
            ]
        }"#;
        let metadata: FeatureMetadata = serde_json::from_str(json).unwrap();
        let mounts = metadata.mounts.unwrap();
        assert_eq!(mounts.len(), 2);
        match &mounts[0] {
            Mount::String(s) => assert!(s.contains("my-vol")),
            _ => panic!("Expected string mount"),
        }
        match &mounts[1] {
            Mount::Object(obj) => {
                assert_eq!(obj.mount_type.as_deref(), Some("tmpfs"));
                assert_eq!(obj.target, "/tmp/scratch");
            }
            _ => panic!("Expected object mount"),
        }
    }

    #[test]
    fn test_merge_feature_properties_lifecycle_commands() {
        let features = vec![
            ResolvedFeature {
                id: "feat-a".to_string(),
                dir: PathBuf::from("/tmp/features/a"),
                options: HashMap::new(),
                metadata: FeatureMetadata {
                    on_create_command: Some(Command::String("echo feat-a-oncreate".to_string())),
                    post_create_command: Some(Command::String(
                        "echo feat-a-postcreate".to_string(),
                    )),
                    ..Default::default()
                },
            },
            ResolvedFeature {
                id: "feat-b".to_string(),
                dir: PathBuf::from("/tmp/features/b"),
                options: HashMap::new(),
                metadata: FeatureMetadata {
                    on_create_command: Some(Command::String("echo feat-b-oncreate".to_string())),
                    post_start_command: Some(Command::String("echo feat-b-poststart".to_string())),
                    post_attach_command: Some(Command::Array(vec![
                        "echo".to_string(),
                        "feat-b-postattach".to_string(),
                    ])),
                    ..Default::default()
                },
            },
        ];
        let result = merge_feature_properties(&features);

        // onCreateCommands: both features contribute, in order
        assert_eq!(result.on_create_commands.len(), 2);
        assert_eq!(
            result.on_create_commands[0],
            Command::String("echo feat-a-oncreate".to_string())
        );
        assert_eq!(
            result.on_create_commands[1],
            Command::String("echo feat-b-oncreate".to_string())
        );

        // postCreateCommands: only feat-a
        assert_eq!(result.post_create_commands.len(), 1);
        assert_eq!(
            result.post_create_commands[0],
            Command::String("echo feat-a-postcreate".to_string())
        );

        // postStartCommands: only feat-b
        assert_eq!(result.post_start_commands.len(), 1);
        assert_eq!(
            result.post_start_commands[0],
            Command::String("echo feat-b-poststart".to_string())
        );

        // postAttachCommands: only feat-b
        assert_eq!(result.post_attach_commands.len(), 1);
        assert_eq!(
            result.post_attach_commands[0],
            Command::Array(vec!["echo".to_string(), "feat-b-postattach".to_string()])
        );
    }

    #[test]
    fn test_merge_feature_properties_no_lifecycle_commands() {
        let features = vec![make_test_feature("a", None), make_test_feature("b", None)];
        let result = merge_feature_properties(&features);
        assert!(result.on_create_commands.is_empty());
        assert!(result.post_create_commands.is_empty());
        assert!(result.post_start_commands.is_empty());
        assert!(result.post_attach_commands.is_empty());
    }

    #[test]
    fn test_feature_metadata_deserialize_lifecycle_commands() {
        let json = r#"{
            "id": "my-feature",
            "onCreateCommand": "echo oncreate",
            "postCreateCommand": ["echo", "postcreate"],
            "postStartCommand": "echo poststart",
            "postAttachCommand": "echo postattach"
        }"#;
        let metadata: FeatureMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(
            metadata.on_create_command,
            Some(Command::String("echo oncreate".to_string()))
        );
        assert_eq!(
            metadata.post_create_command,
            Some(Command::Array(vec![
                "echo".to_string(),
                "postcreate".to_string()
            ]))
        );
        assert_eq!(
            metadata.post_start_command,
            Some(Command::String("echo poststart".to_string()))
        );
        assert_eq!(
            metadata.post_attach_command,
            Some(Command::String("echo postattach".to_string()))
        );
    }

    #[test]
    fn test_has_container_properties_empty() {
        let props = MergedFeatureProperties::default();
        assert!(!props.has_container_properties());
    }

    #[test]
    fn test_has_container_properties_cap_add() {
        let props = MergedFeatureProperties {
            cap_add: vec!["SYS_PTRACE".to_string()],
            ..Default::default()
        };
        assert!(props.has_container_properties());
    }

    #[test]
    fn test_has_container_properties_security_opt() {
        let props = MergedFeatureProperties {
            security_opt: vec!["seccomp=unconfined".to_string()],
            ..Default::default()
        };
        assert!(props.has_container_properties());
    }

    #[test]
    fn test_has_container_properties_init() {
        let props = MergedFeatureProperties {
            init: true,
            ..Default::default()
        };
        assert!(props.has_container_properties());
    }

    #[test]
    fn test_has_container_properties_privileged() {
        let props = MergedFeatureProperties {
            privileged: true,
            ..Default::default()
        };
        assert!(props.has_container_properties());
    }

    #[test]
    fn test_has_container_properties_mounts_only() {
        use devc_config::Mount;
        let props = MergedFeatureProperties {
            mounts: vec![Mount::String(
                "type=volume,source=v,target=/data".to_string(),
            )],
            ..Default::default()
        };
        // Mounts alone don't count as container properties for compose override
        assert!(!props.has_container_properties());
    }

    #[test]
    fn test_has_container_properties_lifecycle_only() {
        let props = MergedFeatureProperties {
            on_create_commands: vec![Command::String("echo hi".to_string())],
            ..Default::default()
        };
        assert!(!props.has_container_properties());
    }

    #[test]
    fn test_feature_metadata_deserialize_remote_env() {
        let json = r#"{"id": "test", "remoteEnv": {"EDITOR": "vim", "PATH": "/custom:${PATH}"}}"#;
        let metadata: FeatureMetadata = serde_json::from_str(json).unwrap();
        let env = metadata.remote_env.unwrap();
        assert_eq!(env.get("EDITOR").unwrap(), "vim");
        assert_eq!(env.get("PATH").unwrap(), "/custom:${PATH}");
    }

    #[test]
    fn test_merge_feature_properties_remote_env() {
        let features = vec![
            ResolvedFeature {
                id: "feat-a".to_string(),
                dir: PathBuf::from("/tmp/features/a"),
                options: HashMap::new(),
                metadata: FeatureMetadata {
                    remote_env: Some({
                        let mut m = HashMap::new();
                        m.insert("EDITOR".to_string(), "nano".to_string());
                        m.insert("FOO".to_string(), "bar".to_string());
                        m
                    }),
                    ..Default::default()
                },
            },
            ResolvedFeature {
                id: "feat-b".to_string(),
                dir: PathBuf::from("/tmp/features/b"),
                options: HashMap::new(),
                metadata: FeatureMetadata {
                    remote_env: Some({
                        let mut m = HashMap::new();
                        // Later feature overrides EDITOR
                        m.insert("EDITOR".to_string(), "vim".to_string());
                        m.insert("BAZ".to_string(), "qux".to_string());
                        m
                    }),
                    ..Default::default()
                },
            },
        ];
        let result = merge_feature_properties(&features);
        assert_eq!(
            result.remote_env.get("EDITOR").unwrap(),
            "vim",
            "later feature should override"
        );
        assert_eq!(result.remote_env.get("FOO").unwrap(), "bar");
        assert_eq!(result.remote_env.get("BAZ").unwrap(), "qux");
        assert_eq!(result.remote_env.len(), 3);
    }

    #[test]
    fn test_merge_feature_properties_no_remote_env() {
        let features = vec![make_test_feature("a", None)];
        let result = merge_feature_properties(&features);
        assert!(result.remote_env.is_empty());
    }

    // --- dependsOn tests ---

    fn make_test_feature_with_depends_on(
        id: &str,
        install_after: Option<Vec<String>>,
        depends_on: Option<HashMap<String, serde_json::Value>>,
    ) -> ResolvedFeature {
        ResolvedFeature {
            id: id.to_string(),
            dir: PathBuf::from(format!("/tmp/features/{}", id)),
            options: HashMap::new(),
            metadata: FeatureMetadata {
                id: Some(id.to_string()),
                install_after,
                depends_on,
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_parse_depends_on_value_empty_object() {
        let val = serde_json::json!({});
        let result = parse_depends_on_value(&val);
        assert_eq!(result, Some(HashMap::new()));
    }

    #[test]
    fn test_parse_depends_on_value_with_options() {
        let val = serde_json::json!({"version": "3", "extra": true});
        let result = parse_depends_on_value(&val).unwrap();
        assert_eq!(result.get("version").unwrap(), "3");
        assert_eq!(result.get("extra").unwrap(), "true");
    }

    #[test]
    fn test_parse_depends_on_value_false_disabled() {
        let val = serde_json::json!(false);
        assert!(parse_depends_on_value(&val).is_none());
    }

    #[test]
    fn test_parse_depends_on_value_true() {
        let val = serde_json::json!(true);
        assert_eq!(parse_depends_on_value(&val), Some(HashMap::new()));
    }

    #[test]
    fn test_feature_metadata_deserialize_depends_on() {
        let json = r#"{
            "id": "my-feature",
            "dependsOn": {
                "ghcr.io/devcontainers/features/common-utils:2": {},
                "./local-dep": {"magicNumber": "42"}
            }
        }"#;
        let metadata: FeatureMetadata = serde_json::from_str(json).unwrap();
        let deps = metadata.depends_on.unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(
            deps.get("ghcr.io/devcontainers/features/common-utils:2")
                .unwrap(),
            &serde_json::json!({})
        );
        assert_eq!(
            deps.get("./local-dep").unwrap(),
            &serde_json::json!({"magicNumber": "42"})
        );
    }

    #[test]
    fn test_order_features_with_depends_on() {
        // Diamond graph: B depends on C and D, C depends on A and E, A depends on E
        let mut b_deps = HashMap::new();
        b_deps.insert("C".to_string(), serde_json::json!({}));
        b_deps.insert("D".to_string(), serde_json::json!({}));
        let mut c_deps = HashMap::new();
        c_deps.insert("A".to_string(), serde_json::json!({}));
        c_deps.insert("E".to_string(), serde_json::json!({}));
        let mut a_deps = HashMap::new();
        a_deps.insert("E".to_string(), serde_json::json!({}));

        let features = vec![
            make_test_feature_with_depends_on("B", None, Some(b_deps)),
            make_test_feature_with_depends_on("C", None, Some(c_deps)),
            make_test_feature_with_depends_on("A", None, Some(a_deps)),
            make_test_feature_with_depends_on("D", None, None),
            make_test_feature_with_depends_on("E", None, None),
        ];

        let ordered = order_features(features).unwrap();
        let ids: Vec<&str> = ordered.iter().map(|f| f.id.as_str()).collect();

        // Verify topological constraints: each feature appears after its deps
        let pos = |id: &str| ids.iter().position(|&x| x == id).unwrap();
        assert!(pos("E") < pos("A"), "E must come before A");
        assert!(pos("E") < pos("C"), "E must come before C");
        assert!(pos("A") < pos("C"), "A must come before C");
        assert!(pos("C") < pos("B"), "C must come before B");
        assert!(pos("D") < pos("B"), "D must come before B");
    }

    #[test]
    fn test_order_features_hard_cycle_detected() {
        // A depends on B, B depends on A → hard cycle
        let mut a_deps = HashMap::new();
        a_deps.insert("B".to_string(), serde_json::json!({}));
        let mut b_deps = HashMap::new();
        b_deps.insert("A".to_string(), serde_json::json!({}));

        let features = vec![
            make_test_feature_with_depends_on("A", None, Some(a_deps)),
            make_test_feature_with_depends_on("B", None, Some(b_deps)),
        ];

        let result = order_features(features);
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            CoreError::FeatureDependencyCycle(msg) => {
                assert!(msg.contains("A"), "error should mention A: {}", msg);
                assert!(msg.contains("B"), "error should mention B: {}", msg);
            }
            other => panic!("Expected FeatureDependencyCycle, got: {:?}", other),
        }
    }

    #[test]
    fn test_order_features_soft_cycle_broken() {
        // A installsAfter B, B installsAfter A → soft cycle, should not error
        let features = vec![
            make_test_feature("A", Some(vec!["B".to_string()])),
            make_test_feature("B", Some(vec!["A".to_string()])),
        ];

        let result = order_features(features);
        assert!(result.is_ok(), "soft cycle should not error");
        let ordered = result.unwrap();
        assert_eq!(ordered.len(), 2);
    }

    #[test]
    fn test_order_features_mixed_hard_soft() {
        // A has dependsOn B (hard), C has installsAfter A (soft)
        let mut a_deps = HashMap::new();
        a_deps.insert("B".to_string(), serde_json::json!({}));

        let features = vec![
            make_test_feature_with_depends_on("A", None, Some(a_deps)),
            make_test_feature_with_depends_on("B", None, None),
            make_test_feature("C", Some(vec!["A".to_string()])),
        ];

        let ordered = order_features(features).unwrap();
        let ids: Vec<&str> = ordered.iter().map(|f| f.id.as_str()).collect();

        let pos = |id: &str| ids.iter().position(|&x| x == id).unwrap();
        assert!(pos("B") < pos("A"), "B must come before A (hard dep)");
        assert!(pos("A") < pos("C"), "A must come before C (soft dep)");
    }
}
