//! Devcontainer features: OCI artifact download, caching, and Dockerfile generation
//!
//! Features are self-contained install scripts distributed as OCI artifacts.
//! This module handles resolving feature references, downloading them,
//! and generating Dockerfile layers for installation.

pub mod compose_override;
pub mod dockerfile;
pub mod download;
pub mod install;
pub mod resolve;

use crate::{CoreError, Result};
use devc_config::FeatureConfig;
use resolve::{
    feature_options, merge_options_with_defaults, order_features, parse_depends_on_value,
    parse_feature_ref, ResolvedFeature,
};
pub use resolve::{merge_feature_properties, MergedFeatureProperties};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tokio::sync::mpsc;

/// Resolve, download, and order all features from a devcontainer config.
///
/// Returns an ordered list of ResolvedFeature ready for Dockerfile generation.
/// Features disabled with `false` are filtered out. Transitive `dependsOn`
/// dependencies are automatically pulled in.
pub async fn resolve_and_prepare_features(
    features: &HashMap<String, FeatureConfig>,
    config_dir: &Path,
    progress: &Option<mpsc::UnboundedSender<String>>,
) -> Result<Vec<ResolvedFeature>> {
    if features.is_empty() {
        return Ok(vec![]);
    }

    // Determine cache directory via GlobalConfig (respects DEVC_CACHE_DIR / DEVC_STATE_DIR)
    let cache_dir = devc_config::GlobalConfig::cache_dir()
        .map(|d| d.join("features"))
        .unwrap_or_else(|_| std::env::temp_dir().join("devc/features"));
    std::fs::create_dir_all(&cache_dir)?;

    // Parse and filter user-requested features
    // user_options tracks options for features explicitly listed by the user
    let mut user_options: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut worklist: Vec<(String, HashMap<String, String>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (id, config) in features {
        let options = match feature_options(config) {
            Some(opts) => opts,
            None => continue, // Bool(false) → skip
        };
        user_options.insert(id.clone(), options.clone());
        worklist.push((id.clone(), options));
        seen.insert(id.clone());
    }

    if worklist.is_empty() {
        return Ok(vec![]);
    }

    // Iterative worklist resolution: download features, discover dependsOn, repeat
    let mut resolved_map: HashMap<String, ResolvedFeature> = HashMap::new();
    // Track declaration order: user features first, then transitive deps in discovery order
    let mut declaration_order: Vec<String> = Vec::new();

    while !worklist.is_empty() {
        // Download all worklist items in parallel
        let mut download_futures = Vec::new();
        for (id, _) in &worklist {
            let id = id.clone();
            let source = parse_feature_ref(&id);
            let cache_dir = cache_dir.clone();
            let config_dir = config_dir.to_path_buf();
            let progress = progress.clone();
            download_futures.push(async move {
                let dir =
                    download::download_feature(&source, &config_dir, &cache_dir, &progress).await?;
                Ok::<(String, std::path::PathBuf), CoreError>((id, dir))
            });
        }

        let results = futures::future::join_all(download_futures).await;
        let mut dir_map: HashMap<String, std::path::PathBuf> = HashMap::new();
        for result in results {
            let (id, dir) = result?;
            dir_map.insert(id, dir);
        }

        // Process downloaded features and discover new dependencies
        let mut next_worklist: Vec<(String, HashMap<String, String>)> = Vec::new();

        for (id, dep_options) in worklist.drain(..) {
            if let Some(dir) = dir_map.get(&id) {
                let metadata = download::read_feature_metadata(dir);

                // Determine final options: user options win over dep-specified options
                let final_options = if let Some(uo) = user_options.get(&id) {
                    merge_options_with_defaults(uo, &metadata)
                } else {
                    merge_options_with_defaults(&dep_options, &metadata)
                };

                // Discover dependsOn deps
                if let Some(ref deps) = metadata.depends_on {
                    for (dep_id, dep_value) in deps {
                        if !seen.contains(dep_id) {
                            if let Some(opts) = parse_depends_on_value(dep_value) {
                                seen.insert(dep_id.clone());
                                next_worklist.push((dep_id.clone(), opts));
                            }
                        }
                    }
                }

                declaration_order.push(id.clone());
                resolved_map.insert(
                    id.clone(),
                    ResolvedFeature {
                        id,
                        dir: dir.clone(),
                        options: final_options,
                        metadata,
                    },
                );
            }
        }

        worklist = next_worklist;
    }

    // Collect in declaration order and sort topologically
    let resolved: Vec<ResolvedFeature> = declaration_order
        .into_iter()
        .filter_map(|id| resolved_map.remove(&id))
        .collect();

    let ordered = order_features(resolved)?;
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_empty_features() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let features = HashMap::new();
        let result = rt.block_on(resolve_and_prepare_features(
            &features,
            Path::new("/tmp"),
            &None,
        ));
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_resolve_all_disabled() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut features = HashMap::new();
        features.insert(
            "ghcr.io/devcontainers/features/node:1".to_string(),
            FeatureConfig::Bool(false),
        );
        let result = rt.block_on(resolve_and_prepare_features(
            &features,
            Path::new("/tmp"),
            &None,
        ));
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_resolve_local_feature() {
        let tmp = tempfile::tempdir().unwrap();
        let feature_dir = tmp.path().join("my-feature");
        std::fs::create_dir_all(&feature_dir).unwrap();
        std::fs::write(feature_dir.join("install.sh"), "#!/bin/bash\necho ok").unwrap();
        std::fs::write(
            feature_dir.join("devcontainer-feature.json"),
            r#"{"id": "my-feature"}"#,
        )
        .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut features = HashMap::new();
        features.insert("./my-feature".to_string(), FeatureConfig::Bool(true));

        let result = rt.block_on(resolve_and_prepare_features(&features, tmp.path(), &None));
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "./my-feature");
    }

    /// Helper to create a local feature directory with metadata JSON
    fn create_local_feature(base: &Path, name: &str, metadata_json: &str) {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("install.sh"), "#!/bin/bash\necho ok").unwrap();
        std::fs::write(dir.join("devcontainer-feature.json"), metadata_json).unwrap();
    }

    #[test]
    fn test_resolve_transitive_local_features() {
        // feature-a depends on ./feature-e. User only requests feature-a.
        // Both should be resolved, with E before A.
        let tmp = tempfile::tempdir().unwrap();

        create_local_feature(
            tmp.path(),
            "feature-a",
            r#"{
                "id": "feature-a",
                "dependsOn": {"./feature-e": {}}
            }"#,
        );
        create_local_feature(tmp.path(), "feature-e", r#"{"id": "feature-e"}"#);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut features = HashMap::new();
        features.insert("./feature-a".to_string(), FeatureConfig::Bool(true));

        let result = rt.block_on(resolve_and_prepare_features(&features, tmp.path(), &None));
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(
            resolved.len(),
            2,
            "Both feature-a and feature-e should be resolved"
        );

        let ids: Vec<&str> = resolved.iter().map(|f| f.id.as_str()).collect();
        let pos_a = ids.iter().position(|&x| x == "./feature-a").unwrap();
        let pos_e = ids.iter().position(|&x| x == "./feature-e").unwrap();
        assert!(
            pos_e < pos_a,
            "feature-e must come before feature-a, got {:?}",
            ids
        );
    }

    #[test]
    fn test_resolve_transitive_chain() {
        // A depends on B, B depends on C (all local). User requests A.
        // All 3 should be resolved in order C, B, A.
        let tmp = tempfile::tempdir().unwrap();

        create_local_feature(
            tmp.path(),
            "feature-a",
            r#"{
                "id": "feature-a",
                "dependsOn": {"./feature-b": {}}
            }"#,
        );
        create_local_feature(
            tmp.path(),
            "feature-b",
            r#"{
                "id": "feature-b",
                "dependsOn": {"./feature-c": {}}
            }"#,
        );
        create_local_feature(tmp.path(), "feature-c", r#"{"id": "feature-c"}"#);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut features = HashMap::new();
        features.insert("./feature-a".to_string(), FeatureConfig::Bool(true));

        let result = rt.block_on(resolve_and_prepare_features(&features, tmp.path(), &None));
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.len(), 3);

        let ids: Vec<&str> = resolved.iter().map(|f| f.id.as_str()).collect();
        let pos = |id: &str| ids.iter().position(|&x| x == id).unwrap();
        assert!(pos("./feature-c") < pos("./feature-b"), "C before B");
        assert!(pos("./feature-b") < pos("./feature-a"), "B before A");
    }

    #[test]
    fn test_resolve_user_options_override_depends_on() {
        // feature-a has dependsOn: {"./feature-b": {"magicNumber": "50"}}
        // User also lists feature-b with magicNumber=99.
        // User's options should win.
        let tmp = tempfile::tempdir().unwrap();

        create_local_feature(
            tmp.path(),
            "feature-a",
            r#"{
                "id": "feature-a",
                "dependsOn": {"./feature-b": {"magicNumber": "50"}}
            }"#,
        );
        create_local_feature(tmp.path(), "feature-b", r#"{"id": "feature-b"}"#);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut features = HashMap::new();
        features.insert("./feature-a".to_string(), FeatureConfig::Bool(true));
        let mut b_opts = HashMap::new();
        b_opts.insert(
            "magicNumber".to_string(),
            serde_json::Value::String("99".to_string()),
        );
        features.insert("./feature-b".to_string(), FeatureConfig::Options(b_opts));

        let result = rt.block_on(resolve_and_prepare_features(&features, tmp.path(), &None));
        assert!(result.is_ok());
        let resolved = result.unwrap();

        let feature_b = resolved.iter().find(|f| f.id == "./feature-b").unwrap();
        assert_eq!(
            feature_b.options.get("magicNumber").unwrap(),
            "99",
            "User's magicNumber=99 should override dep's magicNumber=50"
        );
    }
}
