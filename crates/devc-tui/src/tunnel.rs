//! Port forwarding via direct container exec
//!
//! Uses socat inside the container to forward ports directly through `podman exec`
//! or `docker exec`, without requiring SSH.

use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::task::JoinHandle;

/// Check if socat is installed in a container
pub async fn check_socat_installed(program: &str, prefix: &[String], container_id: &str) -> bool {
    let mut cmd = Command::new(program);
    cmd.args(prefix);
    cmd.args(["exec", container_id, "sh", "-c", "command -v socat"]);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());

    matches!(cmd.status().await, Ok(status) if status.success())
}

/// Result of socat installation attempt
#[derive(Debug)]
pub enum InstallResult {
    Success,
    Failed(String),
    NoPackageManager,
}

/// Package manager definitions for socat installation
pub const PACKAGE_MANAGERS: &[(&str, &str)] = &[
    // Debian/Ubuntu
    ("apt-get", "apt-get update && apt-get install -y socat"),
    // Alpine
    ("apk", "apk add --no-cache socat"),
    // Fedora/RHEL 8+
    ("dnf", "dnf install -y socat"),
    // RHEL 7/CentOS
    ("yum", "yum install -y socat"),
    // Arch
    ("pacman", "pacman -Sy --noconfirm socat"),
];

/// Install socat in a container, detecting the appropriate package manager
pub async fn install_socat(program: &str, prefix: &[String], container_id: &str) -> InstallResult {
    for (pkg_mgr, install_cmd) in PACKAGE_MANAGERS {
        // Check if this package manager exists
        let mut cmd = Command::new(program);
        cmd.args(prefix);
        cmd.args([
            "exec",
            container_id,
            "sh",
            "-c",
            &format!("command -v {}", pkg_mgr),
        ]);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());

        let check = cmd.status().await;

        if matches!(check, Ok(status) if status.success()) {
            // Found package manager, try to install as root
            let mut cmd = Command::new(program);
            cmd.args(prefix);
            cmd.args(["exec", "-u", "root", container_id, "sh", "-c", install_cmd]);

            let output = cmd.output().await;

            return match output {
                Ok(out) if out.status.success() => InstallResult::Success,
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    InstallResult::Failed(format!("Install failed: {}", stderr.trim()))
                }
                Err(e) => InstallResult::Failed(format!("Exec failed: {}", e)),
            };
        }
    }

    InstallResult::NoPackageManager
}

/// Handle to a running port forwarder
pub struct PortForwarder {
    /// Local port on host
    pub local_port: u16,
    /// Remote port in container
    pub remote_port: u16,
    /// Task handle for the listener loop
    listener_handle: JoinHandle<()>,
    /// Shutdown signal sender
    shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
}

impl PortForwarder {
    /// Stop the forwarder and clean up
    pub async fn stop(mut self) {
        // Signal shutdown
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        // Abort the listener task
        self.listener_handle.abort();
        // Wait briefly for cleanup
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            &mut self.listener_handle,
        )
        .await;
    }

    /// Check if still running
    pub fn is_running(&self) -> bool {
        !self.listener_handle.is_finished()
    }
}

impl Drop for PortForwarder {
    fn drop(&mut self) {
        // Signal shutdown (non-blocking)
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        // Abort the listener task - this causes the TcpListener to be dropped,
        // releasing the port. Child processes spawned by handle_connection have
        // kill_on_drop(true), so they'll be terminated when their tasks are aborted.
        self.listener_handle.abort();
    }
}

/// Error type for forwarder operations
#[derive(Debug)]
pub enum ForwarderError {
    /// Port already in use on host
    PortInUse(u16, String),
    /// Failed to spawn exec process
    ExecFailed(String),
    /// socat not found in container
    SocatNotFound,
}

impl std::fmt::Display for ForwarderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwarderError::PortInUse(port, msg) => {
                write!(f, "Port {} already in use on host: {}", port, msg)
            }
            ForwarderError::ExecFailed(msg) => write!(f, "Failed to connect to container: {}", msg),
            ForwarderError::SocatNotFound => {
                write!(
                    f,
                    "socat not found in container. Install with: apt install socat"
                )
            }
        }
    }
}

impl std::error::Error for ForwarderError {}

/// Spawn a port forwarder that forwards connections from localhost to the container
///
/// # Arguments
/// * `program` - Runtime program (e.g. "docker", "flatpak-spawn")
/// * `prefix` - Runtime prefix args (e.g. ["--host", "podman"])
/// * `container_id` - Container ID to forward to
/// * `local_port` - Port on host to listen on
/// * `remote_port` - Port in container to forward to
///
/// # Returns
/// A `PortForwarder` that can be used to monitor and stop the forwarding
pub async fn spawn_forwarder(
    program: String,
    prefix: Vec<String>,
    container_id: String,
    local_port: u16,
    remote_port: u16,
) -> Result<PortForwarder, ForwarderError> {
    // Try to bind the local port
    let listener = TcpListener::bind(format!("127.0.0.1:{}", local_port))
        .await
        .map_err(|e| ForwarderError::PortInUse(local_port, e.to_string()))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let listener_handle = tokio::spawn(async move {
        loop {
            let mut shutdown_rx_clone = shutdown_rx.clone();

            tokio::select! {
                biased;

                _ = shutdown_rx_clone.changed() => {
                    if *shutdown_rx_clone.borrow() {
                        tracing::debug!("Forwarder shutdown signal received");
                        break;
                    }
                }

                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            let cid = container_id.clone();
                            let prog = program.clone();
                            let pfx = prefix.clone();
                            let rp = remote_port;
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, &prog, &pfx, &cid, rp).await {
                                    tracing::debug!("Connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!("Accept error (continuing): {}", e);
                            continue;
                        }
                    }
                }
            }
        }
    });

    Ok(PortForwarder {
        local_port,
        remote_port,
        listener_handle,
        shutdown_tx: Some(shutdown_tx),
    })
}

/// Handle a single TCP connection by forwarding it through container exec
async fn handle_connection(
    tcp_stream: tokio::net::TcpStream,
    program: &str,
    prefix: &[String],
    container_id: &str,
    remote_port: u16,
) -> Result<(), std::io::Error> {
    let socat_cmd = format!("socat - TCP:localhost:{}", remote_port);

    let mut child = Command::new(program)
        .args(prefix)
        .args(["exec", "-i", container_id, "sh", "-c", &socat_cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    let mut child_stdin = child.stdin.take().expect("stdin must exist when piped");
    let mut child_stdout = child.stdout.take().expect("stdout must exist when piped");

    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    // Bidirectional copy using two tasks
    let mut stdin_task = tokio::spawn(async move {
        let result = tokio::io::copy(&mut tcp_read, &mut child_stdin).await;
        // Explicitly close stdin to signal EOF to socat
        let _ = child_stdin.shutdown().await;
        result
    });

    let mut stdout_task =
        tokio::spawn(async move { tokio::io::copy(&mut child_stdout, &mut tcp_write).await });

    // Wait for either direction to complete, then abort the other
    tokio::select! {
        r = &mut stdin_task => {
            stdout_task.abort();
            if let Err(e) = r {
                tracing::debug!("stdin task error: {}", e);
            }
        }
        r = &mut stdout_task => {
            stdin_task.abort();
            if let Err(e) = r {
                tracing::debug!("stdout task error: {}", e);
            }
        }
    }

    // Child process will be killed on drop due to kill_on_drop(true)
    Ok(())
}

/// Spawn the platform-specific browser command for a URL.
fn spawn_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    result.map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether `url` starts with a scheme we'll hand off to the host browser.
pub fn is_browser_openable_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("ftp://")
}

/// Open a full URL in the default browser on the host.
/// Validates that the URL scheme is http, https, or ftp before opening.
pub fn open_url(url: &str) -> Result<(), String> {
    if !is_browser_openable_url(url) {
        return Err(format!(
            "Refusing to open URL with unsupported scheme: {}",
            url
        ));
    }
    spawn_browser(url)
}

/// Open a URL in the default browser
pub fn open_in_browser(port: u16, protocol: Option<&str>) -> Result<(), String> {
    let scheme = if protocol == Some("https") {
        "https"
    } else {
        "http"
    };
    let url = format!("{}://localhost:{}", scheme, port);
    spawn_browser(&url)
}

/// Host literals recognized as "localhost" by `extract_localhost_ports`.
const LOCALHOST_HOSTS: &[&str] = &["127.0.0.1", "localhost", "0.0.0.0", "[::1]"];

/// Extract all localhost port references from a URL, including ones buried
/// inside URL-encoded query-param values. Returns ports in first-seen order,
/// deduplicated.
///
/// Scans for any of `127.0.0.1`, `localhost`, `0.0.0.0`, `[::1]` followed by a
/// port separator (`:` or its URL-encoded form `%3A` / `%3a`) and a run of
/// digits. Intentionally promiscuous: this is called before we open a URL in
/// the host browser and the goal is to pre-spawn a port forwarder for any
/// callback the URL might redirect to — e.g. `aws sso login` opens a URL whose
/// top-level host is `oidc.us-east-1.amazonaws.com` but embeds
/// `redirect_uri=http%3A%2F%2F127.0.0.1%3APORT%2Foauth%2Fcallback` in a query
/// parameter. Without pre-forwarding that callback port, the host browser
/// can't reach the container's OAuth listener when AWS redirects back.
pub fn extract_localhost_ports(url: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = Vec::new();
    for host in LOCALHOST_HOSTS {
        let mut cursor = 0;
        while let Some(rel) = url[cursor..].find(host) {
            let after_host = cursor + rel + host.len();
            let port_start = if url[after_host..].starts_with(':') {
                after_host + 1
            } else if url.len() >= after_host + 3
                && url[after_host..after_host + 3].eq_ignore_ascii_case("%3A")
            {
                after_host + 3
            } else {
                cursor = after_host;
                continue;
            };
            let port_end = url[port_start..]
                .bytes()
                .position(|b| !b.is_ascii_digit())
                .map(|i| port_start + i)
                .unwrap_or(url.len());
            if port_end > port_start {
                if let Ok(port) = url[port_start..port_end].parse::<u16>() {
                    if !ports.contains(&port) {
                        ports.push(port);
                    }
                }
            }
            // port_end is always > cursor: port_start >= after_host + 1 > cursor,
            // and port_end >= port_start. No need for a .max() guard.
            cursor = port_end;
        }
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    /// Helper to check if a port is available (not bound)
    fn port_is_available(port: u16) -> bool {
        std::net::TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok()
    }

    /// Helper to check if a port is listening (can connect)
    fn port_is_listening(port: u16) -> bool {
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port).parse().unwrap(),
            std::time::Duration::from_millis(100),
        )
        .is_ok()
    }

    fn can_bind_localhost() -> bool {
        match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => false,
            Err(_) => false,
        }
    }

    #[tokio::test]
    async fn test_forwarder_binds_port() {
        if !can_bind_localhost() {
            return;
        }
        // Use a high port to avoid conflicts
        let port = 19876;

        // Port should be available before
        assert!(
            port_is_available(port),
            "Port should be available before test"
        );

        // Spawn forwarder (will fail to connect to container, but that's ok - we just want to test port binding)
        let forwarder = spawn_forwarder(
            "docker".to_string(),
            vec![],
            "fake-container".to_string(),
            port,
            3000,
        )
        .await
        .expect("Should bind port");

        // Port should no longer be available (forwarder has it)
        assert!(
            !port_is_available(port),
            "Port should be bound by forwarder"
        );

        // Port should be listening
        assert!(port_is_listening(port), "Forwarder should be listening");

        // Forwarder should report as running
        assert!(forwarder.is_running(), "Forwarder should be running");

        // Stop and verify port is released
        forwarder.stop().await;

        // Give the OS a moment to release the port
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Port should be available again
        assert!(
            port_is_available(port),
            "Port should be released after stop"
        );
    }

    #[tokio::test]
    async fn test_forwarder_drop_releases_port() {
        if !can_bind_localhost() {
            return;
        }
        // Use a different high port
        let port = 19877;

        assert!(
            port_is_available(port),
            "Port should be available before test"
        );

        {
            // Spawn forwarder in a scope
            let forwarder = spawn_forwarder(
                "docker".to_string(),
                vec![],
                "fake-container".to_string(),
                port,
                3000,
            )
            .await
            .expect("Should bind port");

            assert!(!port_is_available(port), "Port should be bound");
            assert!(forwarder.is_running());

            // forwarder is dropped here without calling stop()
        }

        // Give the OS a moment to release the port
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Port should be released by Drop impl
        assert!(
            port_is_available(port),
            "Port should be released after drop"
        );
    }

    #[tokio::test]
    async fn test_port_in_use_error() {
        if !can_bind_localhost() {
            return;
        }
        let port = 19878;

        // Bind the port first
        let _listener =
            std::net::TcpListener::bind(format!("127.0.0.1:{}", port)).expect("Should bind port");

        // Try to spawn forwarder on same port
        let result = spawn_forwarder(
            "docker".to_string(),
            vec![],
            "fake-container".to_string(),
            port,
            3000,
        )
        .await;

        assert!(result.is_err(), "Should fail when port is in use");
        match result {
            Err(ForwarderError::PortInUse(p, _)) => assert_eq!(p, port),
            _ => panic!("Expected PortInUse error"),
        }
    }

    #[tokio::test]
    async fn test_multiple_forwarders_different_ports() {
        if !can_bind_localhost() {
            return;
        }
        let port1 = 19879;
        let port2 = 19880;

        let forwarder1 = spawn_forwarder(
            "docker".to_string(),
            vec![],
            "container1".to_string(),
            port1,
            3000,
        )
        .await
        .expect("Should bind port1");

        let forwarder2 = spawn_forwarder(
            "docker".to_string(),
            vec![],
            "container2".to_string(),
            port2,
            8080,
        )
        .await
        .expect("Should bind port2");

        assert!(forwarder1.is_running());
        assert!(forwarder2.is_running());
        assert!(!port_is_available(port1));
        assert!(!port_is_available(port2));

        // Stop one, verify the other still works
        forwarder1.stop().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(port_is_available(port1), "Port1 should be released");
        assert!(!port_is_available(port2), "Port2 should still be bound");
        assert!(forwarder2.is_running());

        forwarder2.stop().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(port_is_available(port2), "Port2 should be released");
    }

    #[tokio::test]
    async fn test_forwarder_accepts_connections() {
        if !can_bind_localhost() {
            return;
        }
        let port = 19881;

        let forwarder = spawn_forwarder(
            "docker".to_string(),
            vec![],
            "fake-container".to_string(),
            port,
            3000,
        )
        .await
        .expect("Should bind port");

        // Connection should succeed (though it will fail to forward since there's no real container)
        let connect_result = TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port).parse().unwrap(),
            std::time::Duration::from_millis(100),
        );

        assert!(connect_result.is_ok(), "Should accept connection");

        forwarder.stop().await;
    }

    #[test]
    fn test_package_managers_defined() {
        // Verify all expected package managers are defined
        let pkg_mgrs: Vec<&str> = PACKAGE_MANAGERS.iter().map(|(p, _)| *p).collect();
        assert!(pkg_mgrs.contains(&"apt-get"), "Should support apt-get");
        assert!(pkg_mgrs.contains(&"apk"), "Should support apk");
        assert!(pkg_mgrs.contains(&"dnf"), "Should support dnf");
        assert!(pkg_mgrs.contains(&"yum"), "Should support yum");
        assert!(pkg_mgrs.contains(&"pacman"), "Should support pacman");
    }

    #[test]
    fn test_install_commands_contain_socat() {
        // Verify all install commands actually install socat
        for (pkg_mgr, install_cmd) in PACKAGE_MANAGERS {
            assert!(
                install_cmd.contains("socat"),
                "Install command for {} should contain 'socat': {}",
                pkg_mgr,
                install_cmd
            );
        }
    }

    #[test]
    fn test_extract_localhost_ports_plain_localhost() {
        assert_eq!(extract_localhost_ports("http://localhost:3000"), vec![3000]);
        assert_eq!(
            extract_localhost_ports("http://localhost:3000/path"),
            vec![3000]
        );
        assert_eq!(
            extract_localhost_ports("http://localhost:3000?q=1"),
            vec![3000]
        );
        assert_eq!(
            extract_localhost_ports("https://localhost:8443"),
            vec![8443]
        );
        assert_eq!(extract_localhost_ports("http://127.0.0.1:8080"), vec![8080]);
        assert_eq!(
            extract_localhost_ports("https://127.0.0.1:443/foo"),
            vec![443]
        );
        assert_eq!(extract_localhost_ports("http://0.0.0.0:5000"), vec![5000]);
    }

    #[test]
    fn test_extract_localhost_ports_ipv6_localhost() {
        assert_eq!(extract_localhost_ports("http://[::1]:9000"), vec![9000]);
    }

    #[test]
    fn test_extract_localhost_ports_ignores_non_localhost() {
        assert!(extract_localhost_ports("http://example.com:3000").is_empty());
        assert!(extract_localhost_ports("http://192.168.1.1:80").is_empty());
        assert!(extract_localhost_ports("https://oidc.us-east-1.amazonaws.com/").is_empty());
    }

    #[test]
    fn test_extract_localhost_ports_no_port() {
        assert!(extract_localhost_ports("http://localhost").is_empty());
        assert!(extract_localhost_ports("http://localhost/path").is_empty());
        assert!(extract_localhost_ports("not-a-url").is_empty());
    }

    /// AWS SSO login opens a URL whose top-level host is AWS but whose
    /// `redirect_uri` query param (URL-encoded) points at a localhost callback
    /// port in the container. We must extract that port so we can pre-forward
    /// it before the host browser is redirected back.
    #[test]
    fn test_extract_localhost_ports_aws_sso_callback() {
        let url = "https://oidc.us-east-1.amazonaws.com/authorize\
                   ?response_type=code\
                   &client_id=abc\
                   &redirect_uri=http%3A%2F%2F127.0.0.1%3A55512%2Foauth%2Fcallback\
                   &state=xyz";
        assert_eq!(extract_localhost_ports(url), vec![55512]);
    }

    /// URL-encoded colon with lowercase hex (%3a) must also work.
    #[test]
    fn test_extract_localhost_ports_url_encoded_colon_lowercase() {
        let url = "https://example.com/?cb=http%3a%2f%2flocalhost%3a4567%2fdone";
        assert_eq!(extract_localhost_ports(url), vec![4567]);
    }

    /// A URL that mentions localhost at the top level AND in a callback:
    /// return both. Two distinct ports → two entries.
    #[test]
    fn test_extract_localhost_ports_both_top_level_and_query() {
        let url = "http://localhost:3000/auth?redirect=http%3A%2F%2F127.0.0.1%3A4040%2Fcb";
        let ports = extract_localhost_ports(url);
        assert!(ports.contains(&3000), "missing 3000 in {:?}", ports);
        assert!(ports.contains(&4040), "missing 4040 in {:?}", ports);
    }

    /// Same port mentioned twice (once top-level, once in a query) — dedupe.
    #[test]
    fn test_extract_localhost_ports_dedupes_same_port() {
        let url = "http://127.0.0.1:5555/signin?return=http://127.0.0.1:5555/home";
        assert_eq!(extract_localhost_ports(url), vec![5555]);
    }
}
