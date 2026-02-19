//! Feature download: OCI registry and local path handling

use super::resolve::{FeatureMetadata, FeatureSource};
use crate::{CoreError, Result};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use tokio::sync::mpsc;

/// Safely extract a tar archive, rejecting path traversal and absolute paths.
///
/// Iterates entries manually and validates each path before extraction:
/// - Rejects absolute paths
/// - Rejects entries containing `..` components (path traversal)
/// - Rejects symlinks pointing outside the destination directory
fn safe_unpack<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    dest: &Path,
) -> std::result::Result<(), String> {
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;

        // Reject absolute paths
        if path.is_absolute() {
            return Err(format!("Tar contains absolute path: {}", path.display()));
        }

        // Reject path traversal (.. components)
        for component in path.components() {
            if matches!(component, Component::ParentDir) {
                return Err(format!("Tar contains path traversal: {}", path.display()));
            }
        }

        // Reject symlinks that point outside dest
        if entry.header().entry_type().is_symlink() {
            if let Ok(Some(target)) = entry.link_name() {
                // Check for .. in the symlink target
                for component in target.components() {
                    if matches!(component, Component::ParentDir) {
                        return Err(format!(
                            "Tar contains symlink escape: {} -> {}",
                            path.display(),
                            target.display()
                        ));
                    }
                }
                // Also check the resolved path doesn't escape
                if let Some(parent) = dest.join(&path).parent().map(|p| p.to_path_buf()) {
                    let resolved = parent.join(&*target);
                    if resolved.is_absolute() && !resolved.starts_with(dest) {
                        return Err(format!(
                            "Tar contains symlink escape: {} -> {}",
                            path.display(),
                            target.display()
                        ));
                    }
                }
            }
        }

        let target = dest.join(&path);
        // Create parent directories for nested entries
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        entry.unpack(&target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Download or prepare a feature, returning the path to its directory.
///
/// For OCI features, downloads from the registry and caches locally.
/// For local features, validates the path and returns it directly.
/// For tarball URL features, downloads and extracts the tarball.
pub async fn download_feature(
    source: &FeatureSource,
    config_dir: &Path,
    cache_dir: &Path,
    progress: &Option<mpsc::UnboundedSender<String>>,
) -> Result<PathBuf> {
    match source {
        FeatureSource::Oci {
            registry,
            namespace,
            name,
            tag,
        } => download_oci_feature(registry, namespace, name, tag, cache_dir, progress).await,
        FeatureSource::Local { path } => {
            let resolved = if path.is_relative() {
                config_dir.join(path)
            } else {
                path.clone()
            };

            if !resolved.join("install.sh").exists() {
                return Err(CoreError::FeatureDownloadFailed {
                    feature: path.display().to_string(),
                    reason: format!(
                        "Local feature directory missing install.sh: {}",
                        resolved.display()
                    ),
                });
            }

            Ok(resolved)
        }
        FeatureSource::TarballUrl { url } => {
            download_tarball_feature(url, cache_dir, progress).await
        }
    }
}

/// Download an OCI feature artifact from a registry.
async fn download_oci_feature(
    registry: &str,
    namespace: &str,
    name: &str,
    tag: &str,
    cache_dir: &Path,
    progress: &Option<mpsc::UnboundedSender<String>>,
) -> Result<PathBuf> {
    let feature_cache = cache_dir
        .join(registry)
        .join(namespace)
        .join(name)
        .join(tag);

    // Check cache
    if feature_cache.join("install.sh").exists() {
        send_progress(progress, &format!("Feature {}/{}: cached", namespace, name));
        return Ok(feature_cache);
    }

    send_progress(
        progress,
        &format!("Downloading feature {}/{}:{}...", namespace, name, tag),
    );

    let base_url = format!("https://{}", registry);
    let repo = format!("{}/{}", namespace, name);

    let client = reqwest::Client::new();

    // Step 1: Get auth token
    let token = get_auth_token(&client, &base_url, &repo, registry)
        .await
        .map_err(|e| CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}/{}:{}", registry, namespace, name, tag),
            reason: format!("Auth failed: {}", e),
        })?;

    // Step 2: Get manifest
    let manifest_url = format!("{}/v2/{}/manifests/{}", base_url, repo, tag);
    let manifest_resp = client
        .get(&manifest_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await
        .map_err(|e| CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: format!("Manifest request failed: {}", e),
        })?;

    if !manifest_resp.status().is_success() {
        return Err(CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: format!("Manifest fetch returned {}", manifest_resp.status()),
        });
    }

    let manifest: OciManifest =
        manifest_resp
            .json()
            .await
            .map_err(|e| CoreError::FeatureDownloadFailed {
                feature: format!("{}/{}:{}", namespace, name, tag),
                reason: format!("Failed to parse manifest: {}", e),
            })?;

    // Step 3: Find the feature layer
    let layer = manifest
        .layers
        .iter()
        .find(|l| l.media_type == "application/vnd.devcontainers.layer.v1+tar")
        .ok_or_else(|| CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: "No feature layer found in manifest".to_string(),
        })?;

    // Step 4: Download the layer blob
    //
    // Use a no-redirect client because ghcr.io returns a 307 redirect to Azure Blob Storage
    // for blob downloads. If reqwest follows the redirect automatically, it forwards the
    // Authorization header, which Azure rejects (401/403) since it didn't issue that token.
    // We manually follow the redirect without the auth header.
    let blob_url = format!("{}/v2/{}/blobs/{}", base_url, repo, layer.digest);
    let no_redirect_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let blob_resp = no_redirect_client
        .get(&blob_url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: format!("Blob download failed: {}", e),
        })?;

    let blob_resp = if blob_resp.status().is_redirection() {
        // Follow redirect WITHOUT auth header — blob storage doesn't need/want it
        let redirect_url = blob_resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| CoreError::FeatureDownloadFailed {
                feature: format!("{}/{}:{}", namespace, name, tag),
                reason: "Blob redirect missing Location header".to_string(),
            })?
            .to_string();

        reqwest::Client::new()
            .get(&redirect_url)
            .send()
            .await
            .map_err(|e| CoreError::FeatureDownloadFailed {
                feature: format!("{}/{}:{}", namespace, name, tag),
                reason: format!("Blob redirect download failed: {}", e),
            })?
    } else {
        blob_resp
    };

    if !blob_resp.status().is_success() {
        return Err(CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: format!("Blob download returned {}", blob_resp.status()),
        });
    }

    let blob_bytes = blob_resp
        .bytes()
        .await
        .map_err(|e| CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: format!("Failed to read blob: {}", e),
        })?;

    // Step 5: Extract tarball to cache directory
    std::fs::create_dir_all(&feature_cache)?;

    let cursor = std::io::Cursor::new(&blob_bytes);
    let mut archive = tar::Archive::new(cursor);
    safe_unpack(&mut archive, &feature_cache).map_err(|e| {
        // Clean up on failure
        let _ = std::fs::remove_dir_all(&feature_cache);
        CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: format!("Failed to extract tarball: {}", e),
        }
    })?;

    if !feature_cache.join("install.sh").exists() {
        let _ = std::fs::remove_dir_all(&feature_cache);
        return Err(CoreError::FeatureDownloadFailed {
            feature: format!("{}/{}:{}", namespace, name, tag),
            reason: "Extracted tarball does not contain install.sh".to_string(),
        });
    }

    send_progress(
        progress,
        &format!("Feature {}/{}: downloaded", namespace, name),
    );

    Ok(feature_cache)
}

/// Compute a deterministic cache key for a tarball URL.
///
/// Uses `DefaultHasher` (SipHash) for a u64 hash of the URL, formatted as hex.
/// This is sufficient for cache keying — collisions are vanishingly unlikely for URLs.
fn tarball_cache_key(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Returns true if the given bytes start with the gzip magic number (0x1f 0x8b).
fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// Download a feature from an HTTP/HTTPS tarball URL.
///
/// Caches extracted features under `{cache_dir}/urls/{hash}/`.
/// Automatically detects gzip compression via magic bytes.
async fn download_tarball_feature(
    url: &str,
    cache_dir: &Path,
    progress: &Option<mpsc::UnboundedSender<String>>,
) -> Result<PathBuf> {
    // Enforce HTTPS for remote URLs (allow localhost for local development)
    if !url.starts_with("https://")
        && !url.starts_with("http://localhost")
        && !url.starts_with("http://127.0.0.1")
        && !url.starts_with("http://[::1]")
    {
        return Err(CoreError::FeatureDownloadFailed {
            feature: url.to_string(),
            reason: "Only HTTPS URLs are allowed for feature downloads (except localhost)".into(),
        });
    }

    let hash = tarball_cache_key(url);
    let feature_cache = cache_dir.join("urls").join(&hash);

    // Check cache
    if feature_cache.join("install.sh").exists() {
        send_progress(progress, &format!("Feature {}: cached", url));
        return Ok(feature_cache);
    }

    send_progress(progress, &format!("Downloading feature {}...", url));

    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CoreError::FeatureDownloadFailed {
            feature: url.to_string(),
            reason: format!("HTTP request failed: {}", e),
        })?;

    if !resp.status().is_success() {
        return Err(CoreError::FeatureDownloadFailed {
            feature: url.to_string(),
            reason: format!("HTTP {} from {}", resp.status(), url),
        });
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| CoreError::FeatureDownloadFailed {
            feature: url.to_string(),
            reason: format!("Failed to read response body: {}", e),
        })?;

    // Extract tarball (auto-detect gzip)
    std::fs::create_dir_all(&feature_cache)?;

    let extract_result = if is_gzip(&bytes) {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(&bytes));
        let mut archive = tar::Archive::new(decoder);
        safe_unpack(&mut archive, &feature_cache)
    } else {
        let mut archive = tar::Archive::new(std::io::Cursor::new(&bytes));
        safe_unpack(&mut archive, &feature_cache)
    };

    extract_result.map_err(|e| {
        let _ = std::fs::remove_dir_all(&feature_cache);
        CoreError::FeatureDownloadFailed {
            feature: url.to_string(),
            reason: format!("Failed to extract tarball: {}", e),
        }
    })?;

    if !feature_cache.join("install.sh").exists() {
        let _ = std::fs::remove_dir_all(&feature_cache);
        return Err(CoreError::FeatureDownloadFailed {
            feature: url.to_string(),
            reason: "Extracted tarball does not contain install.sh".to_string(),
        });
    }

    send_progress(progress, &format!("Feature {}: downloaded", url));

    Ok(feature_cache)
}

/// Read feature metadata from devcontainer-feature.json in the feature directory.
pub fn read_feature_metadata(feature_dir: &Path) -> FeatureMetadata {
    let metadata_path = feature_dir.join("devcontainer-feature.json");
    if metadata_path.exists() {
        match std::fs::read_to_string(&metadata_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => FeatureMetadata::default(),
        }
    } else {
        FeatureMetadata::default()
    }
}

/// Get an authentication token from the OCI registry.
///
/// Follows the Docker v2 token auth flow:
/// 1. GET /v2/ → 401 with WWW-Authenticate header
/// 2. Parse realm, service from WWW-Authenticate
/// 3. GET <realm>?service=<service>&scope=repository:<repo>:pull
async fn get_auth_token(
    client: &reqwest::Client,
    base_url: &str,
    repo: &str,
    registry: &str,
) -> std::result::Result<String, String> {
    // Try to get the WWW-Authenticate header
    let v2_url = format!("{}/v2/", base_url);
    let resp = client
        .get(&v2_url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach registry: {}", e))?;

    if resp.status() == 200 {
        // No auth needed (unusual but possible)
        return Ok(String::new());
    }

    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| "No WWW-Authenticate header in 401 response".to_string())?
        .to_string();

    let (realm, service) = parse_www_authenticate(&www_auth)?;

    // Try to get credentials from docker config for private registries
    let creds = read_docker_credentials(registry);

    let scope = format!("repository:{}:pull", repo);
    let mut token_req = client
        .get(&realm)
        .query(&[("service", &service), ("scope", &scope)]);

    if let Some((user, pass)) = creds {
        token_req = token_req.basic_auth(user, Some(pass));
    }

    let token_resp = token_req
        .send()
        .await
        .map_err(|e| format!("Token request failed: {}", e))?;

    if !token_resp.status().is_success() {
        return Err(format!("Token endpoint returned {}", token_resp.status()));
    }

    let token_json: serde_json::Value = token_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    token_json
        .get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No token field in response".to_string())
}

/// Parse the WWW-Authenticate header to extract realm and service.
///
/// Format: `Bearer realm="<url>",service="<svc>",...`
fn parse_www_authenticate(header: &str) -> std::result::Result<(String, String), String> {
    let params = header
        .strip_prefix("Bearer ")
        .ok_or_else(|| format!("Unexpected auth scheme: {}", header))?;

    let parsed: HashMap<String, String> = params
        .split(',')
        .filter_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            Some((
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            ))
        })
        .collect();

    let realm = parsed
        .get("realm")
        .ok_or("Missing realm in WWW-Authenticate")?
        .clone();
    let service = parsed
        .get("service")
        .ok_or("Missing service in WWW-Authenticate")?
        .clone();

    Ok((realm, service))
}

/// Read credentials from ~/.docker/config.json for a given registry.
fn read_docker_credentials(registry: &str) -> Option<(String, String)> {
    let home = dirs_for_docker_config()?;
    let config_path = home.join(".docker/config.json");

    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;

    let auth_str = config.get("auths")?.get(registry)?.get("auth")?.as_str()?;

    let decoded =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, auth_str).ok()?;
    let decoded_str = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded_str.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn dirs_for_docker_config() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

fn send_progress(progress: &Option<mpsc::UnboundedSender<String>>, msg: &str) {
    if let Some(ref tx) = progress {
        let _ = tx.send(msg.to_string());
    }
}

/// OCI manifest types (minimal, just what we need)
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciManifest {
    #[allow(dead_code)]
    schema_version: Option<u32>,
    layers: Vec<OciLayer>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciLayer {
    media_type: String,
    digest: String,
    #[allow(dead_code)]
    size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_www_authenticate() {
        let header = r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:devcontainers/features/node:pull""#;
        let (realm, service) = parse_www_authenticate(header).unwrap();
        assert_eq!(realm, "https://ghcr.io/token");
        assert_eq!(service, "ghcr.io");
    }

    #[test]
    fn test_parse_www_authenticate_bad_scheme() {
        let result = parse_www_authenticate("Basic realm=\"foo\"");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_feature_metadata_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = read_feature_metadata(tmp.path());
        assert!(meta.id.is_none());
    }

    #[test]
    fn test_read_feature_metadata_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let meta_json = r#"{
            "id": "node",
            "version": "1.2.3",
            "name": "Node.js",
            "installsAfter": ["common-utils"]
        }"#;
        std::fs::write(tmp.path().join("devcontainer-feature.json"), meta_json).unwrap();

        let meta = read_feature_metadata(tmp.path());
        assert_eq!(meta.id.as_deref(), Some("node"));
        assert_eq!(meta.version.as_deref(), Some("1.2.3"));
        assert_eq!(meta.install_after, Some(vec!["common-utils".to_string()]));
    }

    #[test]
    fn test_local_feature_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let feature_dir = tmp.path().join("my-feature");
        std::fs::create_dir_all(&feature_dir).unwrap();
        std::fs::write(feature_dir.join("install.sh"), "#!/bin/bash\necho hi").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(download_feature(
            &FeatureSource::Local {
                path: PathBuf::from("./my-feature"),
            },
            tmp.path(),
            tmp.path(),
            &None,
        ));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), tmp.path().join("./my-feature"));
    }

    #[test]
    fn test_local_feature_missing_install_sh() {
        let tmp = tempfile::tempdir().unwrap();
        let feature_dir = tmp.path().join("bad-feature");
        std::fs::create_dir_all(&feature_dir).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(download_feature(
            &FeatureSource::Local {
                path: PathBuf::from("./bad-feature"),
            },
            tmp.path(),
            tmp.path(),
            &None,
        ));
        assert!(result.is_err());
    }

    #[test]
    fn test_tarball_cache_key_deterministic() {
        let url = "https://example.com/feature.tar.gz";
        let key1 = tarball_cache_key(url);
        let key2 = tarball_cache_key(url);
        assert_eq!(key1, key2, "Same URL should produce same cache key");
        assert_eq!(key1.len(), 16, "Cache key should be 16 hex chars");
    }

    #[test]
    fn test_tarball_cache_key_different_urls() {
        let key1 = tarball_cache_key("https://example.com/feature-a.tar.gz");
        let key2 = tarball_cache_key("https://example.com/feature-b.tar.gz");
        assert_ne!(
            key1, key2,
            "Different URLs should produce different cache keys"
        );
    }

    #[test]
    fn test_detect_gzip() {
        // Gzip magic bytes
        assert!(is_gzip(&[0x1f, 0x8b, 0x08, 0x00]));
        // Plain tar (starts with filename bytes, not gzip magic)
        assert!(!is_gzip(&[0x66, 0x65, 0x61, 0x74]));
        // Too short
        assert!(!is_gzip(&[0x1f]));
        // Empty
        assert!(!is_gzip(&[]));
    }

    // ==================== safe_unpack tests ====================

    /// Build a raw tar archive with an arbitrary path, bypassing the tar crate's
    /// safety checks. This is needed to test our safe_unpack validation.
    fn build_raw_tar_with_path(path: &str, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();

        // 512-byte tar header
        let mut header = [0u8; 512];
        // Name field: bytes 0..100
        let path_bytes = path.as_bytes();
        header[..path_bytes.len()].copy_from_slice(path_bytes);
        // Mode: bytes 100..108 — "0000644\0"
        header[100..108].copy_from_slice(b"0000644\0");
        // UID: bytes 108..116
        header[108..116].copy_from_slice(b"0001000\0");
        // GID: bytes 116..124
        header[116..124].copy_from_slice(b"0001000\0");
        // Size: bytes 124..136 — octal
        let size_str = format!("{:011o}\0", data.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        // Mtime: bytes 136..148
        header[136..148].copy_from_slice(b"00000000000\0");
        // Typeflag: byte 156 — '0' for regular file
        header[156] = b'0';
        // Magic: bytes 257..263
        header[257..263].copy_from_slice(b"ustar\0");
        // Version: bytes 263..265
        header[263..265].copy_from_slice(b"00");

        // Compute checksum: sum of all header bytes, treating checksum field (148..156) as spaces
        header[148..156].copy_from_slice(b"        ");
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(cksum_str.as_bytes());

        buf.extend_from_slice(&header);
        buf.extend_from_slice(data);
        // Pad data to 512-byte boundary
        let padding = (512 - (data.len() % 512)) % 512;
        buf.extend(std::iter::repeat(0u8).take(padding));
        // Two 512-byte zero blocks as end-of-archive marker
        buf.extend(std::iter::repeat(0u8).take(1024));
        buf
    }

    #[test]
    fn test_safe_unpack_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_bytes = build_raw_tar_with_path("../escape.txt", b"malicious content");

        let mut archive = tar::Archive::new(std::io::Cursor::new(&tar_bytes));
        let result = safe_unpack(&mut archive, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("path traversal"));
    }

    #[test]
    fn test_safe_unpack_rejects_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_bytes = build_raw_tar_with_path("/etc/evil", b"malicious content");

        let mut archive = tar::Archive::new(std::io::Cursor::new(&tar_bytes));
        let result = safe_unpack(&mut archive, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("absolute path"));
    }

    #[test]
    fn test_safe_unpack_allows_normal_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_bytes = {
            let buf = Vec::new();
            let mut archive = tar::Builder::new(buf);

            let data = b"#!/bin/bash\necho hi";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, "install.sh", &data[..])
                .unwrap();

            let data2 = b"nested content";
            let mut header2 = tar::Header::new_gnu();
            header2.set_size(data2.len() as u64);
            header2.set_mode(0o644);
            header2.set_cksum();
            archive
                .append_data(&mut header2, "subdir/file.txt", &data2[..])
                .unwrap();

            archive.into_inner().unwrap()
        };

        let mut archive = tar::Archive::new(std::io::Cursor::new(&tar_bytes));
        let result = safe_unpack(&mut archive, tmp.path());
        assert!(result.is_ok());
        assert!(tmp.path().join("install.sh").exists());
        assert!(tmp.path().join("subdir/file.txt").exists());
    }

    #[test]
    fn test_safe_unpack_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_bytes = {
            let buf = Vec::new();
            let mut archive = tar::Builder::new(buf);

            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            archive
                .append_link(&mut header, "evil-link", "../../../etc/passwd")
                .unwrap();

            archive.into_inner().unwrap()
        };

        let mut archive = tar::Archive::new(std::io::Cursor::new(&tar_bytes));
        let result = safe_unpack(&mut archive, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("symlink escape"));
    }

    // ==================== HTTPS enforcement tests ====================

    #[tokio::test]
    async fn test_tarball_url_rejects_http() {
        let cache_dir = tempfile::tempdir().unwrap();
        let result =
            download_tarball_feature("http://example.com/f.tar.gz", cache_dir.path(), &None).await;
        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(err.contains("HTTPS"));
    }

    #[tokio::test]
    async fn test_tarball_url_allows_https() {
        let cache_dir = tempfile::tempdir().unwrap();
        // This will fail on network (no server), but should NOT fail on URL validation
        let result = download_tarball_feature(
            "https://example.com/feature.tar.gz",
            cache_dir.path(),
            &None,
        )
        .await;
        // Should fail with a network error, not a URL validation error
        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(!err.contains("HTTPS"), "Should not fail on URL validation");
    }

    #[tokio::test]
    async fn test_tarball_url_allows_localhost_http() {
        let cache_dir = tempfile::tempdir().unwrap();
        // These should pass URL validation (will fail on network since no server)
        let result =
            download_tarball_feature("http://localhost:8080/f.tar.gz", cache_dir.path(), &None)
                .await;
        let err = format!("{:?}", result.unwrap_err());
        assert!(
            !err.contains("HTTPS"),
            "localhost should be allowed over HTTP"
        );

        let result =
            download_tarball_feature("http://127.0.0.1:8080/f.tar.gz", cache_dir.path(), &None)
                .await;
        let err = format!("{:?}", result.unwrap_err());
        assert!(
            !err.contains("HTTPS"),
            "127.0.0.1 should be allowed over HTTP"
        );
    }

    #[tokio::test]
    async fn test_tarball_download_and_extract() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tokio::io::AsyncWriteExt;

        // Create a tar.gz in memory with install.sh and devcontainer-feature.json
        let tar_bytes = {
            let buf = Vec::new();
            let encoder = GzEncoder::new(buf, Compression::default());
            let mut archive = tar::Builder::new(encoder);

            let install_sh = b"#!/bin/bash\necho hello\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(install_sh.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, "install.sh", &install_sh[..])
                .unwrap();

            let metadata = br#"{"id": "test-tarball", "version": "1.0.0"}"#;
            let mut header = tar::Header::new_gnu();
            header.set_size(metadata.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "devcontainer-feature.json", &metadata[..])
                .unwrap();

            archive.into_inner().unwrap().finish().unwrap()
        };

        // Start a local HTTP server
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // Some sandboxes disallow binding local sockets for tests.
                return;
            }
            Err(e) => panic!("failed to bind local test server: {}", e),
        };
        let addr = listener.local_addr().unwrap();
        let url = format!("http://127.0.0.1:{}/feature.tar.gz", addr.port());

        let tar_bytes_clone = tar_bytes.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Read the HTTP request (drain it)
            let mut buf = vec![0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            // Write HTTP response
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/gzip\r\n\r\n",
                tar_bytes_clone.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&tar_bytes_clone).await.unwrap();
            socket.flush().await.unwrap();
        });

        let cache_dir = tempfile::tempdir().unwrap();
        let result = download_tarball_feature(&url, cache_dir.path(), &None).await;

        server.await.unwrap();

        let feature_dir = result.expect("download should succeed");
        assert!(feature_dir.join("install.sh").exists());
        assert!(feature_dir.join("devcontainer-feature.json").exists());

        let metadata = read_feature_metadata(&feature_dir);
        assert_eq!(metadata.id.as_deref(), Some("test-tarball"));

        // Verify caching: second call should return cached path
        // (server is gone so it would fail if it tried to download again)
        let result2 = download_tarball_feature(&url, cache_dir.path(), &None).await;
        assert_eq!(result2.unwrap(), feature_dir);
    }
}
