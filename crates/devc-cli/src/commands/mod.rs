//! CLI command implementations

mod lifecycle;
mod manage;

use anyhow::{anyhow, Result};
use devc_core::{display_name_map, ContainerManager, ContainerState};

pub use lifecycle::*;
pub use manage::*;

/// Find a container by name or ID
async fn find_container(manager: &ContainerManager, name_or_id: &str) -> Result<ContainerState> {
    // Try by ID first (exact match — UUIDs from selector)
    if let Some(state) = manager.get(name_or_id).await? {
        return Ok(state);
    }

    // Try by name (user typed a name on the command line)
    if let Some(state) = manager.get_by_name(name_or_id).await? {
        return Ok(state);
    }

    // Try partial ID match
    let containers = manager.list().await?;
    let matches: Vec<_> = containers
        .iter()
        .filter(|c| c.id.starts_with(name_or_id) || c.name.starts_with(name_or_id))
        .collect();

    match matches.len() {
        0 => Err(anyhow!("Container '{}' not found", name_or_id)),
        1 => Ok(matches[0].clone()),
        _ => Err(anyhow!(
            "Ambiguous container reference '{}', matches: {}",
            name_or_id,
            {
                let all = manager.list().await?;
                let display = display_name_map(&all);
                matches
                    .iter()
                    .map(|c| {
                        display
                            .get(&c.id)
                            .cloned()
                            .unwrap_or_else(|| c.name.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )),
    }
}

/// Find container for current working directory
async fn find_container_in_cwd(manager: &ContainerManager) -> Result<ContainerState> {
    let cwd = std::env::current_dir()?;
    let containers = manager.list().await?;

    containers
        .into_iter()
        .find(|c| c.workspace_path == cwd)
        .ok_or_else(|| anyhow!("No container found for current directory"))
}

/// Execute a shell script in a container and return stdout
async fn exec_check(
    provider: &dyn devc_provider::ContainerProvider,
    cid: &devc_provider::ContainerId,
    script: &str,
    user: Option<&str>,
) -> Option<String> {
    let result = provider
        .exec(
            cid,
            &devc_provider::ExecConfig {
                cmd: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
                env: std::collections::HashMap::new(),
                working_dir: None,
                user: user.map(|s| s.to_string()),
                tty: false,
                stdin: false,
                privileged: false,
            },
        )
        .await
        .ok()?;
    if result.exit_code != 0 || result.output.trim().is_empty() {
        return None;
    }
    Some(result.output)
}
