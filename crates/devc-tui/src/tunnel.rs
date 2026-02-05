//! Port forwarding via direct container exec
//!
//! Uses socat inside the container to forward ports directly through `podman exec`
//! or `docker exec`, without requiring SSH.

use devc_provider::ProviderType;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::task::JoinHandle;

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
                write!(f, "socat not found in container. Install with: apt install socat")
            }
        }
    }
}

impl std::error::Error for ForwarderError {}

/// Spawn a port forwarder that forwards connections from localhost to the container
///
/// # Arguments
/// * `provider_type` - Docker or Podman
/// * `container_id` - Container ID to forward to
/// * `local_port` - Port on host to listen on
/// * `remote_port` - Port in container to forward to
///
/// # Returns
/// A `PortForwarder` that can be used to monitor and stop the forwarding
pub async fn spawn_forwarder(
    provider_type: ProviderType,
    container_id: &str,
    local_port: u16,
    remote_port: u16,
) -> Result<PortForwarder, ForwarderError> {
    // Try to bind the local port
    let listener = TcpListener::bind(format!("127.0.0.1:{}", local_port))
        .await
        .map_err(|e| ForwarderError::PortInUse(local_port, e.to_string()))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let container_id_owned = container_id.to_string();

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
                            let cid = container_id_owned.clone();
                            let pt = provider_type;
                            let rp = remote_port;
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, pt, &cid, rp).await {
                                    tracing::debug!("Connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Accept error: {}", e);
                            break;
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
    provider_type: ProviderType,
    container_id: &str,
    remote_port: u16,
) -> Result<(), std::io::Error> {
    // Build exec command
    let (cmd, args) = build_exec_command(provider_type, container_id, remote_port);

    let mut child = Command::new(&cmd)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    let mut child_stdin = child.stdin.take().unwrap();
    let mut child_stdout = child.stdout.take().unwrap();

    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    // Bidirectional copy using two tasks
    let stdin_task = tokio::spawn(async move {
        let result = tokio::io::copy(&mut tcp_read, &mut child_stdin).await;
        // Explicitly close stdin to signal EOF to socat
        let _ = child_stdin.shutdown().await;
        result
    });

    let stdout_task = tokio::spawn(async move {
        tokio::io::copy(&mut child_stdout, &mut tcp_write).await
    });

    // Wait for either direction to complete (connection closed)
    tokio::select! {
        r = stdin_task => {
            if let Err(e) = r {
                tracing::debug!("stdin task error: {}", e);
            }
        }
        r = stdout_task => {
            if let Err(e) = r {
                tracing::debug!("stdout task error: {}", e);
            }
        }
    }

    // Child process will be killed on drop due to kill_on_drop(true)
    Ok(())
}

/// Build the exec command for forwarding via socat
fn build_exec_command(
    provider_type: ProviderType,
    container_id: &str,
    remote_port: u16,
) -> (String, Vec<String>) {
    let socat_cmd = format!("socat - TCP:localhost:{}", remote_port);

    match provider_type {
        ProviderType::Docker => (
            "docker".to_string(),
            vec![
                "exec".to_string(),
                "-i".to_string(),
                container_id.to_string(),
                "sh".to_string(),
                "-c".to_string(),
                socat_cmd,
            ],
        ),
        ProviderType::Podman => {
            // Check if running in flatpak
            if std::env::var("FLATPAK_ID").is_ok() {
                (
                    "flatpak-spawn".to_string(),
                    vec![
                        "--host".to_string(),
                        "podman".to_string(),
                        "exec".to_string(),
                        "-i".to_string(),
                        container_id.to_string(),
                        "sh".to_string(),
                        "-c".to_string(),
                        socat_cmd,
                    ],
                )
            } else {
                (
                    "podman".to_string(),
                    vec![
                        "exec".to_string(),
                        "-i".to_string(),
                        container_id.to_string(),
                        "sh".to_string(),
                        "-c".to_string(),
                        socat_cmd,
                    ],
                )
            }
        }
    }
}

/// Open a URL in the default browser
pub fn open_in_browser(port: u16) -> Result<(), String> {
    let url = format!("http://localhost:{}", port);

    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", &url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    result.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn test_build_exec_command_docker() {
        let (cmd, args) = build_exec_command(ProviderType::Docker, "abc123", 3000);
        assert_eq!(cmd, "docker");
        assert_eq!(
            args,
            vec!["exec", "-i", "abc123", "sh", "-c", "socat - TCP:localhost:3000"]
        );
    }

    #[test]
    fn test_build_exec_command_podman() {
        // Clear FLATPAK_ID if set
        std::env::remove_var("FLATPAK_ID");

        let (cmd, args) = build_exec_command(ProviderType::Podman, "def456", 8080);
        assert_eq!(cmd, "podman");
        assert_eq!(
            args,
            vec!["exec", "-i", "def456", "sh", "-c", "socat - TCP:localhost:8080"]
        );
    }

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

    #[tokio::test]
    async fn test_forwarder_binds_port() {
        // Use a high port to avoid conflicts
        let port = 19876;

        // Port should be available before
        assert!(port_is_available(port), "Port should be available before test");

        // Spawn forwarder (will fail to connect to container, but that's ok - we just want to test port binding)
        let forwarder = spawn_forwarder(ProviderType::Docker, "fake-container", port, 3000)
            .await
            .expect("Should bind port");

        // Port should no longer be available (forwarder has it)
        assert!(!port_is_available(port), "Port should be bound by forwarder");

        // Port should be listening
        assert!(port_is_listening(port), "Forwarder should be listening");

        // Forwarder should report as running
        assert!(forwarder.is_running(), "Forwarder should be running");

        // Stop and verify port is released
        forwarder.stop().await;

        // Give the OS a moment to release the port
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Port should be available again
        assert!(port_is_available(port), "Port should be released after stop");
    }

    #[tokio::test]
    async fn test_forwarder_drop_releases_port() {
        // Use a different high port
        let port = 19877;

        assert!(port_is_available(port), "Port should be available before test");

        {
            // Spawn forwarder in a scope
            let forwarder = spawn_forwarder(ProviderType::Docker, "fake-container", port, 3000)
                .await
                .expect("Should bind port");

            assert!(!port_is_available(port), "Port should be bound");
            assert!(forwarder.is_running());

            // forwarder is dropped here without calling stop()
        }

        // Give the OS a moment to release the port
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Port should be released by Drop impl
        assert!(port_is_available(port), "Port should be released after drop");
    }

    #[tokio::test]
    async fn test_port_in_use_error() {
        let port = 19878;

        // Bind the port first
        let _listener = std::net::TcpListener::bind(format!("127.0.0.1:{}", port))
            .expect("Should bind port");

        // Try to spawn forwarder on same port
        let result = spawn_forwarder(ProviderType::Docker, "fake-container", port, 3000).await;

        assert!(result.is_err(), "Should fail when port is in use");
        match result {
            Err(ForwarderError::PortInUse(p, _)) => assert_eq!(p, port),
            _ => panic!("Expected PortInUse error"),
        }
    }

    #[tokio::test]
    async fn test_multiple_forwarders_different_ports() {
        let port1 = 19879;
        let port2 = 19880;

        let forwarder1 = spawn_forwarder(ProviderType::Docker, "container1", port1, 3000)
            .await
            .expect("Should bind port1");

        let forwarder2 = spawn_forwarder(ProviderType::Docker, "container2", port2, 8080)
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
        let port = 19881;

        let forwarder = spawn_forwarder(ProviderType::Docker, "fake-container", port, 3000)
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
}
