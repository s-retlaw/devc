//! Port detection for containers
//!
//! Detects listening ports inside containers by parsing /proc/net/tcp

use devc_provider::{ContainerId, ContainerProvider, ExecConfig, ProviderType};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// A detected port in a container
#[derive(Debug, Clone)]
pub struct DetectedPort {
    /// Port number
    pub port: u16,
    /// Protocol (tcp/udp)
    pub protocol: String,
    /// Process name if detectable
    pub process: Option<String>,
    /// Whether this port was newly detected (for [NEW] indicator)
    pub is_new: bool,
    /// Whether this port is currently being forwarded
    pub is_forwarded: bool,
}

/// Parse /proc/net/tcp or /proc/net/tcp6 to find listening ports
///
/// Format of /proc/net/tcp:
/// ```text
///   sl  local_address rem_address   st tx_queue rx_queue ...
///    0: 00000000:0050 00000000:0000 0A 00000000:00000000 ...
/// ```
/// - local_address is in hex format: ADDRESS:PORT
/// - st (state) 0A = LISTEN
pub fn parse_proc_net_tcp(data: &str) -> Vec<u16> {
    data.lines()
        .skip(1) // Skip header
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 4 {
                return None;
            }
            // State 0A = LISTEN
            if fields[3] != "0A" {
                return None;
            }
            // local_address format: ADDR:PORT (hex)
            let local = fields[1];
            let port_hex = local.split(':').nth(1)?;
            u16::from_str_radix(port_hex, 16).ok()
        })
        .collect()
}

/// Detect listening ports in a container via exec
pub async fn detect_ports(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
) -> Result<Vec<u16>, String> {
    let config = ExecConfig {
        cmd: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "cat /proc/net/tcp /proc/net/tcp6 2>/dev/null || true".to_string(),
        ],
        env: HashMap::new(),
        working_dir: None,
        user: Some("root".to_string()),
        tty: false,
        stdin: false,
        privileged: false,
    };

    let result = provider.exec(container_id, &config).await.map_err(|e| e.to_string())?;

    if result.exit_code != 0 {
        return Err(format!("exec failed with code {}", result.exit_code));
    }

    let mut ports: Vec<u16> = parse_proc_net_tcp(&result.output);
    // Remove duplicates and sort
    ports.sort();
    ports.dedup();
    // Filter out common system ports that are usually not interesting
    ports.retain(|&p| p >= 1024 || p == 22 || p == 80 || p == 443);

    Ok(ports)
}

/// Port detection update message
#[derive(Debug, Clone)]
pub struct PortDetectionUpdate {
    /// Updated list of detected ports
    pub ports: Vec<DetectedPort>,
}

/// Spawn a background task that periodically detects ports in a container
///
/// Returns a receiver that will receive port detection updates.
/// The task will exit when the receiver is dropped.
pub fn spawn_port_detector(
    provider: Arc<dyn ContainerProvider + Send + Sync>,
    container_id: ContainerId,
    _provider_type: ProviderType,
    forwarded_ports: HashSet<u16>,
) -> mpsc::UnboundedReceiver<PortDetectionUpdate> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut last_ports: HashSet<u16> = HashSet::new();
        let mut iteration = 0;

        loop {
            iteration += 1;

            match detect_ports(provider.as_ref(), &container_id).await {
                Ok(ports) => {
                    let current: HashSet<u16> = ports.iter().copied().collect();
                    let new_ports: HashSet<u16> = current.difference(&last_ports).copied().collect();

                    // Only mark as new on first detection after initial scan
                    let detected: Vec<DetectedPort> = ports
                        .into_iter()
                        .map(|port| DetectedPort {
                            port,
                            protocol: "tcp".to_string(),
                            process: None, // Process detection would require additional exec
                            is_new: iteration > 1 && new_ports.contains(&port),
                            is_forwarded: forwarded_ports.contains(&port),
                        })
                        .collect();

                    let update = PortDetectionUpdate { ports: detected };
                    if tx.send(update).is_err() {
                        // Receiver dropped, exit task
                        break;
                    }
                    last_ports = current;
                }
                Err(e) => {
                    tracing::debug!("Port detection error: {}", e);
                    // Continue polling even on errors
                }
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        tracing::debug!(
            "Port detector task exiting for container {}",
            container_id.short()
        );
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proc_net_tcp() {
        let data = r#"  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0
   1: 00000000:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12346 1 0000000000000000 100 0 0 10 0
   2: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12347 1 0000000000000000 100 0 0 10 0
   3: 0100007F:1F40 0100007F:0BB8 01 00000000:00000000 00:00000000 00000000  1000        0 12348 1 0000000000000000 100 0 0 10 0"#;

        let ports = parse_proc_net_tcp(data);
        // 0016 = 22 (ssh), 0050 = 80 (http), 0BB8 = 3000
        // Line 3 is in state 01 (ESTABLISHED), not 0A (LISTEN), so should be excluded
        assert_eq!(ports, vec![22, 80, 3000]);
    }

    #[test]
    fn test_parse_proc_net_tcp_empty() {
        let data = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        let ports = parse_proc_net_tcp(data);
        assert!(ports.is_empty());
    }

    #[test]
    fn test_parse_proc_net_tcp_malformed() {
        let data = "malformed data\nno valid lines here";
        let ports = parse_proc_net_tcp(data);
        assert!(ports.is_empty());
    }
}
