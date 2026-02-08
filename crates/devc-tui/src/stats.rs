//! Background stats polling for container resource monitoring

use devc_provider::{ContainerId, ContainerProvider, ContainerStats};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Spawn a background task that polls container stats periodically.
///
/// Returns a receiver that yields stats updates every ~3 seconds.
/// The task stops when the receiver is dropped.
pub fn spawn_stats_poller(
    provider: Arc<dyn ContainerProvider>,
    container_ids: Vec<ContainerId>,
) -> mpsc::UnboundedReceiver<Vec<ContainerStats>> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let id_refs: Vec<&ContainerId> = container_ids.iter().collect();
            if let Ok(stats) = provider.stats(&id_refs).await {
                if tx.send(stats).is_err() {
                    // Receiver dropped, stop polling
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    });

    rx
}
