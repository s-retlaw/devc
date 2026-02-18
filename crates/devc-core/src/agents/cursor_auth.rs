use rusqlite::Connection;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct CursorTokens {
    pub auth_token: String,
    pub refresh_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorTokenSource {
    AuthJson,
    StateVscdb,
    MacKeychain,
    WindowsCredentialManager,
    WslWindowsPath,
}

impl CursorTokenSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CursorTokenSource::AuthJson => "auth.json",
            CursorTokenSource::StateVscdb => "state.vscdb",
            CursorTokenSource::MacKeychain => "macos-keychain",
            CursorTokenSource::WindowsCredentialManager => "windows-credential-manager",
            CursorTokenSource::WslWindowsPath => "wsl-windows-profile",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CursorAuthResolution {
    pub source: CursorTokenSource,
    pub tokens: CursorTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeMode {
    NativeWindows,
    MacOs,
    Linux,
    Wsl,
}

pub fn resolve_cursor_tokens() -> Result<CursorAuthResolution, String> {
    let mode = runtime_mode();

    let auth_candidates = auth_json_candidates(mode);
    if let Some((tokens, is_wsl_windows)) = first_auth_json_tokens(&auth_candidates) {
        return Ok(CursorAuthResolution {
            source: if is_wsl_windows {
                CursorTokenSource::WslWindowsPath
            } else {
                CursorTokenSource::AuthJson
            },
            tokens,
        });
    }

    let vscdb_candidates = state_vscdb_candidates(mode);
    if let Some((tokens, is_wsl_windows)) = first_state_db_tokens(&vscdb_candidates) {
        return Ok(CursorAuthResolution {
            source: if is_wsl_windows {
                CursorTokenSource::WslWindowsPath
            } else {
                CursorTokenSource::StateVscdb
            },
            tokens,
        });
    }

    match mode {
        RuntimeMode::MacOs => {
            if let Some(tokens) = resolve_macos_keychain_tokens() {
                return Ok(CursorAuthResolution {
                    source: CursorTokenSource::MacKeychain,
                    tokens,
                });
            }
        }
        RuntimeMode::NativeWindows => {
            if let Some(tokens) = resolve_windows_credential_manager_tokens() {
                return Ok(CursorAuthResolution {
                    source: CursorTokenSource::WindowsCredentialManager,
                    tokens,
                });
            }
        }
        RuntimeMode::Linux | RuntimeMode::Wsl => {}
    }

    Err(format!(
        "no Cursor tokens found (checked auth.json, state.vscdb{}{})",
        if mode == RuntimeMode::MacOs {
            ", macOS keychain"
        } else {
            ""
        },
        if mode == RuntimeMode::NativeWindows {
            ", Windows Credential Manager"
        } else {
            ""
        }
    ))
}

fn runtime_mode() -> RuntimeMode {
    #[cfg(target_os = "windows")]
    {
        return RuntimeMode::NativeWindows;
    }
    #[cfg(target_os = "macos")]
    {
        return RuntimeMode::MacOs;
    }
    #[cfg(target_os = "linux")]
    {
        return if is_wsl() {
            RuntimeMode::Wsl
        } else {
            RuntimeMode::Linux
        };
    }
    #[allow(unreachable_code)]
    RuntimeMode::Linux
}

#[cfg(target_os = "linux")]
fn is_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/version")
        .map(|v| v.to_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn is_wsl() -> bool {
    false
}

fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

fn auth_json_candidates(mode: RuntimeMode) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match mode {
        RuntimeMode::MacOs => {
            if let Some(home) = home_dir() {
                out.push(home.join("Library/Application Support/cursor/auth.json"));
                out.push(home.join("Library/Application Support/Cursor/auth.json"));
            }
        }
        RuntimeMode::NativeWindows => {
            if let Some(appdata) = std::env::var_os("APPDATA") {
                let base = PathBuf::from(appdata);
                out.push(base.join("cursor/auth.json"));
                out.push(base.join("Cursor/auth.json"));
            }
        }
        RuntimeMode::Linux | RuntimeMode::Wsl => {
            if let Some(home) = home_dir() {
                out.push(home.join(".config/cursor/auth.json"));
                out.push(home.join(".config/Cursor/auth.json"));
            }
        }
    }

    if mode == RuntimeMode::Wsl {
        out.extend(wsl_windows_profile_paths("auth.json"));
    }

    dedup_paths(out)
}

fn state_vscdb_candidates(mode: RuntimeMode) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match mode {
        RuntimeMode::MacOs => {
            if let Some(home) = home_dir() {
                out.push(
                    home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
                );
            }
        }
        RuntimeMode::NativeWindows => {
            if let Some(appdata) = std::env::var_os("APPDATA") {
                let base = PathBuf::from(appdata);
                out.push(base.join("Cursor/User/globalStorage/state.vscdb"));
            }
        }
        RuntimeMode::Linux | RuntimeMode::Wsl => {
            if let Some(home) = home_dir() {
                out.push(home.join(".config/Cursor/User/globalStorage/state.vscdb"));
            }
        }
    }

    if mode == RuntimeMode::Wsl {
        out.extend(wsl_windows_profile_paths("User/globalStorage/state.vscdb"));
    }

    dedup_paths(out)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

fn is_windows_profile_path(path: &Path) -> bool {
    path.starts_with("/mnt/c/Users/")
}

#[cfg(target_os = "linux")]
fn wsl_windows_profile_paths(suffix: &str) -> Vec<PathBuf> {
    if !is_wsl() {
        return Vec::new();
    }

    let mut users = Vec::new();
    if let Ok(output) = Command::new("cmd.exe")
        .args(["/C", "echo", "%USERNAME%"])
        .output()
    {
        if output.status.success() {
            let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !user.is_empty() {
                users.push(user);
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir("/mnt/c/Users") {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if !matches!(name, "All Users" | "Default" | "Default User" | "Public") {
                        users.push(name.to_string());
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    for user in users {
        out.push(PathBuf::from(format!(
            "/mnt/c/Users/{}/AppData/Roaming/Cursor/{}",
            user, suffix
        )));
    }
    dedup_paths(out)
}

#[cfg(not(target_os = "linux"))]
fn wsl_windows_profile_paths(_suffix: &str) -> Vec<PathBuf> {
    Vec::new()
}

#[derive(Debug, Deserialize)]
struct AuthJson {
    #[serde(rename = "accessToken")]
    auth_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
}

fn first_auth_json_tokens(paths: &[PathBuf]) -> Option<(CursorTokens, bool)> {
    for p in paths {
        if let Some(tokens) = read_auth_json_tokens(p) {
            return Some((tokens, is_windows_profile_path(p)));
        }
    }
    None
}

fn read_auth_json_tokens(path: &Path) -> Option<CursorTokens> {
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: AuthJson = serde_json::from_str(&content).ok()?;
    let auth = parsed.auth_token?.trim().to_string();
    let refresh = parsed.refresh_token?.trim().to_string();
    if auth.is_empty() || refresh.is_empty() {
        return None;
    }
    Some(CursorTokens {
        auth_token: auth,
        refresh_token: refresh,
    })
}

fn first_state_db_tokens(paths: &[PathBuf]) -> Option<(CursorTokens, bool)> {
    for p in paths {
        if let Some(tokens) = read_state_vscdb_tokens(p) {
            return Some((tokens, is_windows_profile_path(p)));
        }
    }
    None
}

fn read_state_vscdb_tokens(path: &Path) -> Option<CursorTokens> {
    let auth = query_state_value(path, "cursorAuth/accessToken")?;
    let refresh = query_state_value(path, "cursorAuth/refreshToken")?;
    if auth.trim().is_empty() || refresh.trim().is_empty() {
        return None;
    }
    Some(CursorTokens {
        auth_token: auth,
        refresh_token: refresh,
    })
}

fn query_state_value(path: &Path, key: &str) -> Option<String> {
    let conn = Connection::open(path).ok()?;
    let value: String = conn
        .query_row("SELECT value FROM ItemTable WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .ok()?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(value)
}

#[cfg(target_os = "macos")]
fn resolve_macos_keychain_tokens() -> Option<CursorTokens> {
    let access = first_keychain_value(&[
        ("Cursor", "cursorAuth/accessToken"),
        ("Cursor", "accessToken"),
        ("cursor", "cursorAuth/accessToken"),
        ("cursor", "accessToken"),
    ])?;
    let refresh = first_keychain_value(&[
        ("Cursor", "cursorAuth/refreshToken"),
        ("Cursor", "refreshToken"),
        ("cursor", "cursorAuth/refreshToken"),
        ("cursor", "refreshToken"),
    ])?;
    Some(CursorTokens {
        auth_token: access,
        refresh_token: refresh,
    })
}

#[cfg(not(target_os = "macos"))]
fn resolve_macos_keychain_tokens() -> Option<CursorTokens> {
    None
}

#[cfg(target_os = "macos")]
fn first_keychain_value(candidates: &[(&str, &str)]) -> Option<String> {
    for (service, account) in candidates {
        let output = Command::new("security")
            .args(["find-generic-password", "-s", service, "-a", account, "-w"])
            .output()
            .ok()?;
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn resolve_windows_credential_manager_tokens() -> Option<CursorTokens> {
    let script = r#"
$ErrorActionPreference='SilentlyContinue'
$vault = New-Object Windows.Security.Credentials.PasswordVault
$items = $vault.RetrieveAll()
$auth = ''
$refresh = ''
foreach ($item in $items) {
  $item.RetrievePassword()
  $meta = ($item.Resource + ' ' + $item.UserName).ToLower()
  if (-not $auth -and $meta -match 'cursor' -and ($meta -match 'access' -or $meta -match 'auth')) { $auth = $item.Password }
  if (-not $refresh -and $meta -match 'cursor' -and $meta -match 'refresh') { $refresh = $item.Password }
}
if ($auth) { Write-Output ('AUTH=' + $auth) }
if ($refresh) { Write-Output ('REFRESH=' + $refresh) }
"#;

    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let auth = stdout
        .lines()
        .find_map(|l| l.strip_prefix("AUTH="))
        .map(|s| s.trim().to_string())?;
    let refresh = stdout
        .lines()
        .find_map(|l| l.strip_prefix("REFRESH="))
        .map(|s| s.trim().to_string())?;
    if auth.is_empty() || refresh.is_empty() {
        return None;
    }
    Some(CursorTokens {
        auth_token: auth,
        refresh_token: refresh,
    })
}

#[cfg(not(target_os = "windows"))]
fn resolve_windows_credential_manager_tokens() -> Option<CursorTokens> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_read_auth_json_tokens() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{"accessToken":"auth-token-123","refreshToken":"refresh-token-456"}"#,
        )
        .unwrap();

        let tokens = read_auth_json_tokens(&path).expect("should parse auth.json");
        assert_eq!(tokens.auth_token, "auth-token-123");
        assert_eq!(tokens.refresh_token, "refresh-token-456");
    }

    #[test]
    fn test_read_state_vscdb_tokens() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("state.vscdb");
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            ("cursorAuth/accessToken", "a-token"),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            ("cursorAuth/refreshToken", "r-token"),
        )
        .unwrap();

        let tokens = read_state_vscdb_tokens(&path).expect("should parse db tokens");
        assert_eq!(tokens.auth_token, "a-token");
        assert_eq!(tokens.refresh_token, "r-token");
    }
}
