//! Docker registry credential support
//!
//! Parses ~/.docker/config.json and invokes credential helpers to get
//! registry authentication for private registries.

use base64::Engine;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

/// Docker configuration from ~/.docker/config.json
#[derive(Debug, Deserialize, Default)]
pub struct DockerConfig {
    #[serde(default)]
    pub auths: HashMap<String, AuthEntry>,
    #[serde(rename = "credsStore")]
    pub creds_store: Option<String>,
    #[serde(rename = "credHelpers", default)]
    pub cred_helpers: HashMap<String, String>,
}

/// Auth entry in the auths section
#[derive(Debug, Deserialize, Default)]
pub struct AuthEntry {
    /// Base64-encoded "username:password"
    pub auth: Option<String>,
}

/// Response from credential helper
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CredentialResponse {
    username: String,
    secret: String,
}

impl DockerConfig {
    /// Load Docker config from ~/.docker/config.json
    pub fn load() -> Option<Self> {
        let config_path = dirs::home_dir()?.join(".docker/config.json");
        let content = std::fs::read_to_string(&config_path).ok()?;
        match serde_json::from_str(&content) {
            Ok(config) => Some(config),
            Err(e) => {
                tracing::warn!("Failed to parse docker config: {}", e);
                None
            }
        }
    }

    /// Get all available registry credentials
    pub fn get_all_credentials(&self) -> HashMap<String, (String, String)> {
        let mut credentials = HashMap::new();

        // Collect credentials for all known registries
        for registry in self.auths.keys() {
            if let Some(cred) = self.get_credential(registry) {
                credentials.insert(registry.clone(), cred);
            }
        }

        credentials
    }

    /// Get credential for a specific registry
    pub fn get_credential(&self, registry: &str) -> Option<(String, String)> {
        // Check for registry-specific credential helper first
        if let Some(helper) = self.cred_helpers.get(registry) {
            if let Some(cred) = invoke_credential_helper(helper, registry) {
                return Some(cred);
            }
        }

        // Try global credential helper
        if let Some(store) = &self.creds_store {
            if let Some(cred) = invoke_credential_helper(store, registry) {
                return Some(cred);
            }
        }

        // Fall back to static auths
        if let Some(entry) = self.auths.get(registry) {
            if let Some(auth) = &entry.auth {
                return decode_auth(auth);
            }
        }

        None
    }
}

/// Invoke a Docker credential helper to get credentials for a registry
fn invoke_credential_helper(store: &str, registry: &str) -> Option<(String, String)> {
    // Helper binary is named "docker-credential-{store}"
    let helper = format!("docker-credential-{}", store);

    let mut child = match Command::new(&helper)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            tracing::debug!("Failed to spawn credential helper '{}': {}", helper, e);
            return None;
        }
    };

    // Write registry to stdin
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(registry.as_bytes()).is_err() {
            tracing::debug!("Failed to write to credential helper stdin");
            return None;
        }
    }

    // Wait for output
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(e) => {
            tracing::debug!("Credential helper failed: {}", e);
            return None;
        }
    };

    if !output.status.success() {
        tracing::debug!(
            "Credential helper '{}' returned non-zero for registry '{}'",
            helper,
            registry
        );
        return None;
    }

    // Parse JSON response: {"Username": "...", "Secret": "..."}
    match serde_json::from_slice::<CredentialResponse>(&output.stdout) {
        Ok(response) => Some((response.username, response.secret)),
        Err(e) => {
            tracing::debug!("Failed to parse credential helper response: {}", e);
            None
        }
    }
}

/// Decode base64-encoded "username:password" auth string
fn decode_auth(auth: &str) -> Option<(String, String)> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(auth)
        .ok()?;
    let decoded_str = String::from_utf8(decoded).ok()?;
    let parts: Vec<&str> = decoded_str.splitn(2, ':').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

/// Get registry authentication for build operations
/// Returns credentials for all configured registries
pub fn get_registry_auth() -> Option<HashMap<String, bollard::auth::DockerCredentials>> {
    let config = DockerConfig::load()?;
    let mut creds = HashMap::new();

    // Collect credentials for all known registries
    for (registry, (username, password)) in config.get_all_credentials() {
        creds.insert(
            registry,
            bollard::auth::DockerCredentials {
                username: Some(username),
                password: Some(password),
                ..Default::default()
            },
        );
    }

    if creds.is_empty() {
        None
    } else {
        Some(creds)
    }
}

/// Get registry authentication for a specific image
/// Extracts the registry from the image name and returns credentials if available
pub fn get_credential_for_image(image: &str) -> Option<bollard::auth::DockerCredentials> {
    let registry = extract_registry_from_image(image)?;
    let config = DockerConfig::load()?;
    let (username, password) = config.get_credential(&registry)?;

    Some(bollard::auth::DockerCredentials {
        username: Some(username),
        password: Some(password),
        serveraddress: Some(registry),
        ..Default::default()
    })
}

/// Extract registry hostname from an image name
/// Examples:
///   "nginx" -> None (Docker Hub, no auth needed typically)
///   "registry.example.com/image:tag" -> Some("registry.example.com")
///   "registry.example.com:5000/image" -> Some("registry.example.com:5000")
fn extract_registry_from_image(image: &str) -> Option<String> {
    // Remove digest suffix first (@sha256:...)
    let image_no_digest = image.split('@').next().unwrap_or(image);

    // Split by '/'
    let parts: Vec<&str> = image_no_digest.split('/').collect();

    if parts.len() < 2 {
        // Single component like "nginx" or "nginx:latest" - Docker Hub
        return None;
    }

    let first = parts[0];

    // Check if first component looks like a registry (has a dot or colon, or is "localhost")
    if first.contains('.') || first.contains(':') || first == "localhost" {
        Some(first.to_string())
    } else {
        // No registry prefix (e.g., "library/nginx") - Docker Hub
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_registry_from_image() {
        assert_eq!(extract_registry_from_image("nginx"), None);
        assert_eq!(extract_registry_from_image("nginx:latest"), None);
        assert_eq!(extract_registry_from_image("library/nginx"), None);

        assert_eq!(
            extract_registry_from_image("registry.example.com/myimage"),
            Some("registry.example.com".to_string())
        );
        assert_eq!(
            extract_registry_from_image("registry.example.com/org/myimage:v1"),
            Some("registry.example.com".to_string())
        );
        assert_eq!(
            extract_registry_from_image("localhost:5000/myimage"),
            Some("localhost:5000".to_string())
        );
        assert_eq!(
            extract_registry_from_image("gcr.io/project/image"),
            Some("gcr.io".to_string())
        );
    }

    #[test]
    fn test_decode_auth() {
        // "testuser:testpass" in base64
        let encoded = base64::engine::general_purpose::STANDARD.encode("testuser:testpass");
        let result = decode_auth(&encoded);
        assert_eq!(
            result,
            Some(("testuser".to_string(), "testpass".to_string()))
        );
    }

    #[test]
    fn test_decode_auth_with_colon_in_password() {
        // "user:pass:word" - password contains colon
        let encoded = base64::engine::general_purpose::STANDARD.encode("user:pass:word");
        let result = decode_auth(&encoded);
        assert_eq!(result, Some(("user".to_string(), "pass:word".to_string())));
    }
}
