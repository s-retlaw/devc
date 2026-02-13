//! Host-side credential resolution
//!
//! Reads Docker and Git credentials from the host system by parsing config
//! files and invoking credential helpers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// Timeout for credential helper invocations
const HELPER_TIMEOUT: Duration = Duration::from_secs(5);

/// A resolved Docker auth entry (base64-encoded user:pass)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerAuth {
    pub auth: String,
}

/// Parsed Docker config.json structure
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct DockerCredConfig {
    /// Default credential store (e.g. "desktop", "secretservice", "osxkeychain")
    #[serde(default)]
    pub creds_store: Option<String>,
    /// Per-registry credential helpers
    #[serde(default)]
    pub cred_helpers: HashMap<String, String>,
    /// Inline base64-encoded credentials
    #[serde(default)]
    pub auths: HashMap<String, AuthEntry>,
}

/// An entry in Docker config.json "auths"
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthEntry {
    pub auth: Option<String>,
}

/// Credential helper JSON response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CredHelperResponse {
    #[serde(alias = "ServerURL")]
    server_url: Option<String>,
    #[serde(alias = "Username")]
    username: Option<String>,
    #[serde(alias = "Secret")]
    secret: Option<String>,
}

/// A resolved Git credential
#[derive(Debug, Clone)]
pub struct GitCredential {
    pub protocol: String,
    pub host: String,
    pub username: String,
    pub password: String,
}

/// Read and parse ~/.docker/config.json
pub fn read_docker_cred_config() -> Option<DockerCredConfig> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    let config_path = docker_config_path(&home);

    let content = std::fs::read_to_string(&config_path).ok()?;
    serde_json::from_str(&content).ok()
}

fn docker_config_path(home: &Path) -> PathBuf {
    // Respect DOCKER_CONFIG env var
    if let Ok(docker_config) = std::env::var("DOCKER_CONFIG") {
        return PathBuf::from(docker_config).join("config.json");
    }
    home.join(".docker/config.json")
}

/// Resolve all Docker credentials from the host.
///
/// Collects credentials from credsStore, credHelpers, and auths entries.
/// Returns a map of registry → base64-encoded auth strings.
pub async fn resolve_docker_credentials() -> HashMap<String, DockerAuth> {
    let config = match read_docker_cred_config() {
        Some(c) => c,
        None => {
            tracing::debug!("No Docker config found, skipping Docker credential resolution");
            return HashMap::new();
        }
    };

    let mut result = HashMap::new();

    // 1. Resolve inline auths (lowest priority)
    for (registry, entry) in &config.auths {
        if let Some(ref auth) = entry.auth {
            if !auth.is_empty() {
                result.insert(
                    registry.clone(),
                    DockerAuth {
                        auth: auth.clone(),
                    },
                );
            }
        }
    }

    // 2. Resolve per-registry credHelpers (overrides auths)
    for (registry, helper) in &config.cred_helpers {
        if let Some(auth) = resolve_docker_credential_helper(helper, registry).await {
            result.insert(registry.clone(), auth);
        }
    }

    // 3. Resolve credsStore for any auths registries not yet covered by credHelpers
    if let Some(ref store) = config.creds_store {
        if !store.is_empty() {
            // Resolve for all known registries from auths that don't have a credHelper
            for registry in config.auths.keys() {
                if !config.cred_helpers.contains_key(registry) {
                    if let Some(auth) = resolve_docker_credential_helper(store, registry).await {
                        result.insert(registry.clone(), auth);
                    }
                }
            }
            // Also try the default Docker Hub registry
            if !result.contains_key("https://index.docker.io/v1/") {
                if let Some(auth) =
                    resolve_docker_credential_helper(store, "https://index.docker.io/v1/").await
                {
                    result.insert("https://index.docker.io/v1/".to_string(), auth);
                }
            }
        }
    }

    result
}

/// Validate a Docker credential helper name.
///
/// Helper names like "desktop", "ecr-login", "osxkeychain" should only
/// contain alphanumeric chars, hyphens, and underscores.
fn is_valid_helper_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Call `docker-credential-<helper> get` with registry on stdin
async fn resolve_docker_credential_helper(
    helper: &str,
    registry: &str,
) -> Option<DockerAuth> {
    if !is_valid_helper_name(helper) {
        tracing::warn!(
            "Skipping invalid Docker credential helper name: {:?}",
            helper
        );
        return None;
    }

    let binary = format!("docker-credential-{}", helper);
    tracing::debug!("Calling {} get for {}", binary, registry);

    let result = tokio::time::timeout(
        HELPER_TIMEOUT,
        async {
            let mut child = Command::new(&binary)
                .arg("get")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;

            // Write registry to stdin
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(registry.as_bytes()).await.ok()?;
                drop(stdin);
            }

            let output = child.wait_with_output().await.ok()?;
            if !output.status.success() {
                return None;
            }

            let response: CredHelperResponse =
                serde_json::from_slice(&output.stdout).ok()?;

            let username = response.username?;
            let secret = response.secret?;

            // Encode as base64 auth string
            let auth = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                format!("{}:{}", username, secret),
            );

            Some(DockerAuth { auth })
        },
    )
    .await;

    match result {
        Ok(auth) => auth,
        Err(_) => {
            tracing::warn!("Timeout calling docker-credential-{} for {}", helper, registry);
            None
        }
    }
}

/// Resolve a Git credential for a specific protocol+host.
///
/// Calls `git credential fill` on the host.
pub async fn resolve_git_credential(
    protocol: &str,
    host: &str,
) -> Option<GitCredential> {
    let input = format!("protocol={}\nhost={}\n\n", protocol, host);

    let result = tokio::time::timeout(HELPER_TIMEOUT, async {
        let mut child = Command::new("git")
            .args(["credential", "fill"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(input.as_bytes()).await.ok()?;
            drop(stdin);
        }

        let output = child.wait_with_output().await.ok()?;
        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8(output.stdout).ok()?;
        parse_git_credential_output(&stdout, protocol, host)
    })
    .await;

    match result {
        Ok(cred) => cred,
        Err(_) => {
            tracing::warn!("Timeout resolving git credential for {}://{}", protocol, host);
            None
        }
    }
}

/// Parse git credential fill output into a GitCredential
fn parse_git_credential_output(
    output: &str,
    protocol: &str,
    host: &str,
) -> Option<GitCredential> {
    let mut username = None;
    let mut password = None;

    for line in output.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "username" => username = Some(value.to_string()),
                "password" => password = Some(value.to_string()),
                _ => {}
            }
        }
    }

    Some(GitCredential {
        protocol: protocol.to_string(),
        host: host.to_string(),
        username: username?,
        password: password?,
    })
}

/// Resolve Git credentials for well-known hosts
pub async fn resolve_git_credentials() -> Vec<GitCredential> {
    let hosts = [
        ("https", "github.com"),
        ("https", "gitlab.com"),
        ("https", "bitbucket.org"),
        ("https", "dev.azure.com"),
    ];

    let mut credentials = Vec::new();
    for (protocol, host) in &hosts {
        if let Some(cred) = resolve_git_credential(protocol, host).await {
            credentials.push(cred);
        }
    }
    credentials
}

/// Format git credentials as a git-credentials store file
pub fn format_git_credentials(credentials: &[GitCredential]) -> String {
    credentials
        .iter()
        .map(|c| {
            format!(
                "{}://{}:{}@{}",
                c.protocol,
                urlencoded(&c.username),
                urlencoded(&c.password),
                c.host,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// URL encoding for credential strings used in git-credentials file format.
///
/// Must encode all characters that are special in URLs to ensure safe
/// parsing by the shell helper's sed patterns.
fn urlencoded(s: &str) -> String {
    s.replace('%', "%25")
        .replace(':', "%3A")
        .replace('@', "%40")
        .replace('/', "%2F")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('?', "%3F")
        .replace('&', "%26")
        .replace('+', "%2B")
        .replace('=', "%3D")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
}

/// Build Docker config.json content from resolved credentials
pub fn build_docker_config_json(auths: &HashMap<String, DockerAuth>) -> String {
    let mut auth_map = serde_json::Map::new();
    for (registry, docker_auth) in auths {
        let mut entry = serde_json::Map::new();
        entry.insert(
            "auth".to_string(),
            serde_json::Value::String(docker_auth.auth.clone()),
        );
        auth_map.insert(
            registry.clone(),
            serde_json::Value::Object(entry),
        );
    }

    let mut config = serde_json::Map::new();
    config.insert(
        "auths".to_string(),
        serde_json::Value::Object(auth_map),
    );

    serde_json::to_string_pretty(&config).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_docker_cred_config() {
        let json = r#"{
            "auths": {
                "ghcr.io": { "auth": "dXNlcjpwYXNz" },
                "docker.io": {}
            },
            "credsStore": "desktop",
            "credHelpers": {
                "ecr.aws": "ecr-login"
            }
        }"#;

        let config: DockerCredConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.creds_store, Some("desktop".to_string()));
        assert_eq!(config.cred_helpers.get("ecr.aws"), Some(&"ecr-login".to_string()));
        assert_eq!(
            config.auths.get("ghcr.io").unwrap().auth,
            Some("dXNlcjpwYXNz".to_string())
        );
        assert!(config.auths.get("docker.io").unwrap().auth.is_none());
    }

    #[test]
    fn test_parse_docker_cred_config_empty() {
        let json = "{}";
        let config: DockerCredConfig = serde_json::from_str(json).unwrap();
        assert!(config.creds_store.is_none());
        assert!(config.cred_helpers.is_empty());
        assert!(config.auths.is_empty());
    }

    #[test]
    fn test_parse_git_credential_output() {
        let output = "protocol=https\nhost=github.com\nusername=user\npassword=ghp_token123\n";
        let cred = parse_git_credential_output(output, "https", "github.com").unwrap();
        assert_eq!(cred.username, "user");
        assert_eq!(cred.password, "ghp_token123");
        assert_eq!(cred.protocol, "https");
        assert_eq!(cred.host, "github.com");
    }

    #[test]
    fn test_parse_git_credential_output_missing_password() {
        let output = "protocol=https\nhost=github.com\nusername=user\n";
        assert!(parse_git_credential_output(output, "https", "github.com").is_none());
    }

    #[test]
    fn test_parse_git_credential_output_missing_username() {
        let output = "protocol=https\nhost=github.com\npassword=token\n";
        assert!(parse_git_credential_output(output, "https", "github.com").is_none());
    }

    #[test]
    fn test_format_git_credentials() {
        let creds = vec![
            GitCredential {
                protocol: "https".to_string(),
                host: "github.com".to_string(),
                username: "user".to_string(),
                password: "token123".to_string(),
            },
            GitCredential {
                protocol: "https".to_string(),
                host: "gitlab.com".to_string(),
                username: "other".to_string(),
                password: "pass".to_string(),
            },
        ];
        let formatted = format_git_credentials(&creds);
        assert_eq!(
            formatted,
            "https://user:token123@github.com\nhttps://other:pass@gitlab.com"
        );
    }

    #[test]
    fn test_format_git_credentials_special_chars() {
        let creds = vec![GitCredential {
            protocol: "https".to_string(),
            host: "github.com".to_string(),
            username: "user@org".to_string(),
            password: "p@ss:word/test".to_string(),
        }];
        let formatted = format_git_credentials(&creds);
        assert_eq!(
            formatted,
            "https://user%40org:p%40ss%3Aword%2Ftest@github.com"
        );
    }

    #[test]
    fn test_build_docker_config_json() {
        let mut auths = HashMap::new();
        auths.insert(
            "ghcr.io".to_string(),
            DockerAuth {
                auth: "dXNlcjpwYXNz".to_string(),
            },
        );

        let json = build_docker_config_json(&auths);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["auths"]["ghcr.io"]["auth"].as_str().unwrap(),
            "dXNlcjpwYXNz"
        );
    }

    #[test]
    fn test_build_docker_config_json_empty() {
        let json = build_docker_config_json(&HashMap::new());
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["auths"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_urlencoded() {
        assert_eq!(urlencoded("simple"), "simple");
        assert_eq!(urlencoded("user@org"), "user%40org");
        assert_eq!(urlencoded("p:w"), "p%3Aw");
        assert_eq!(urlencoded("a/b"), "a%2Fb");
        assert_eq!(urlencoded("a b"), "a%20b");
        assert_eq!(urlencoded("100%"), "100%25");
        assert_eq!(urlencoded("pass#word"), "pass%23word");
        assert_eq!(urlencoded("a?b&c=d+e"), "a%3Fb%26c%3Dd%2Be");
        assert_eq!(urlencoded("line\nnew"), "line%0Anew");
    }

    #[test]
    fn test_is_valid_helper_name() {
        assert!(is_valid_helper_name("desktop"));
        assert!(is_valid_helper_name("ecr-login"));
        assert!(is_valid_helper_name("osxkeychain"));
        assert!(is_valid_helper_name("dev-containers-abc123"));
        assert!(!is_valid_helper_name(""));
        assert!(!is_valid_helper_name("foo;rm -rf /"));
        assert!(!is_valid_helper_name("../../evil"));
        assert!(!is_valid_helper_name("helper name"));
    }
}
