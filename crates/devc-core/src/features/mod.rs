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
use resolve::{feature_options, merge_options_with_defaults, order_features, parse_feature_ref, ResolvedFeature};
pub use resolve::{merge_feature_properties, MergedFeatureProperties};
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

/// Resolve, download, and order all features from a devcontainer config.
///
/// Returns an ordered list of ResolvedFeature ready for Dockerfile generation.
/// Features disabled with `false` are filtered out.
pub async fn resolve_and_prepare_features(
    features: &HashMap<String, FeatureConfig>,
    config_dir: &Path,
    progress: &Option<mpsc::UnboundedSender<String>>,
) -> Result<Vec<ResolvedFeature>> {
    if features.is_empty() {
        return Ok(vec![]);
    }

    // Determine cache directory
    let cache_dir = directories::BaseDirs::new()
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir())
        .join("devc/features");
    std::fs::create_dir_all(&cache_dir)?;

    // Parse and filter features
    let mut pending = Vec::new();
    for (id, config) in features {
        let options = match feature_options(config) {
            Some(opts) => opts,
            None => continue, // Bool(false) → skip
        };
        let source = parse_feature_ref(id);
        pending.push((id.clone(), source, options));
    }

    if pending.is_empty() {
        return Ok(vec![]);
    }

    // Download all features (in parallel)
    let mut download_futures = Vec::new();
    for (id, source, _) in &pending {
        let id = id.clone();
        let source = source.clone();
        let cache_dir = cache_dir.clone();
        let config_dir = config_dir.to_path_buf();
        let progress = progress.clone();
        download_futures.push(async move {
            let dir = download::download_feature(&source, &config_dir, &cache_dir, &progress).await?;
            Ok::<(String, std::path::PathBuf), CoreError>((id, dir))
        });
    }

    let results = futures::future::join_all(download_futures).await;

    // Collect results, mapping id → dir
    let mut dir_map: HashMap<String, std::path::PathBuf> = HashMap::new();
    for result in results {
        let (id, dir) = result?;
        dir_map.insert(id, dir);
    }

    // Build ResolvedFeature list (preserving declaration order)
    // Merge user-provided options with defaults from feature metadata
    let mut resolved = Vec::new();
    for (id, _, user_options) in pending {
        if let Some(dir) = dir_map.get(&id) {
            let metadata = download::read_feature_metadata(dir);
            let options = merge_options_with_defaults(&user_options, &metadata);
            resolved.push(ResolvedFeature {
                id,
                dir: dir.clone(),
                options,
                metadata,
            });
        }
    }

    // Order features respecting installsAfter
    let ordered = order_features(resolved);

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
        features.insert(
            "./my-feature".to_string(),
            FeatureConfig::Bool(true),
        );

        let result = rt.block_on(resolve_and_prepare_features(
            &features,
            tmp.path(),
            &None,
        ));
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "./my-feature");
    }
}
