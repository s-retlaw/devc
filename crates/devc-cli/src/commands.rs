//! CLI command implementations

use anyhow::{anyhow, bail, Context, Result};
use devc_config::GlobalConfig;
use devc_core::{ContainerManager, ContainerState, DevcContainerStatus};

/// Run a command in a container
pub async fn run(manager: &ContainerManager, container: &str, cmd: Vec<String>) -> Result<()> {
    let state = find_container(manager, container).await?;

    if state.status != DevcContainerStatus::Running {
        bail!("Container '{}' is not running", state.name);
    }

    if cmd.is_empty() {
        bail!("No command specified");
    }

    let exit_code = manager.exec(&state.id, cmd.clone(), true).await?;

    if exit_code != 0 {
        std::process::exit(exit_code as i32);
    }

    Ok(())
}

/// Open an interactive shell in a container
pub async fn shell(manager: &ContainerManager, container: &str) -> Result<()> {
    let state = find_container(manager, container).await?;

    if state.status != DevcContainerStatus::Running {
        // Try to start it first
        if state.status == DevcContainerStatus::Stopped || state.status == DevcContainerStatus::Created {
            println!("Starting container '{}'...", state.name);
            manager.start(&state.id).await?;
            // Re-fetch state after starting
            let state = find_container(manager, container).await?;
            return ssh_to_container(&state).await;
        } else {
            bail!("Container '{}' is not running (status: {})", state.name, state.status);
        }
    }

    ssh_to_container(&state).await
}

/// Connect to container, preferring SSH over stdio when available
async fn ssh_to_container(state: &ContainerState) -> Result<()> {
    let container_id = state.container_id.as_ref()
        .ok_or_else(|| anyhow!("Container has no container ID"))?;

    // Check if SSH is available for this container
    let ssh_available = state.metadata.get("ssh_available")
        .map(|v| v == "true")
        .unwrap_or(false);

    if ssh_available {
        match ssh_via_dropbear(state).await {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                // SSH exited with non-zero but we should still respect that
                std::process::exit(status.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("Warning: SSH connection failed ({}), falling back to exec", e);
                eprintln!("Note: Terminal resize will not work with exec fallback");
            }
        }
    }

    // Fallback to podman/docker exec -it
    exec_shell_fallback(state, container_id).await
}

/// Connect via SSH over stdio using dropbear in inetd mode
async fn ssh_via_dropbear(state: &ContainerState) -> Result<std::process::ExitStatus> {
    let container_id = state.container_id.as_ref()
        .ok_or_else(|| anyhow!("Container has no container ID"))?;

    let user = state.metadata.get("remote_user")
        .map(|s| s.as_str())
        .unwrap_or("root");

    // Get the SSH key path
    let key_path = GlobalConfig::data_dir()
        .map(|d| d.join("ssh/id_ed25519"))
        .context("Failed to get data directory")?;

    if !key_path.exists() {
        bail!("SSH key not found at {:?}", key_path);
    }

    // Build the proxy command based on environment
    // Dropbear runs as daemon on 127.0.0.1:2222, we use socat to connect
    // Shell-quote container_id for safe interpolation into ProxyCommand
    let quoted_id = format!("'{}'", container_id.replace('\'', "'\\''"));
    let proxy_cmd = if is_in_toolbox() {
        format!(
            "flatpak-spawn --host podman exec -i {} socat - TCP:127.0.0.1:2222",
            quoted_id
        )
    } else {
        let runtime = match state.provider {
            devc_provider::ProviderType::Docker => "docker",
            devc_provider::ProviderType::Podman => "podman",
        };
        format!(
            "{} exec -i {} socat - TCP:127.0.0.1:2222",
            runtime, quoted_id
        )
    };

    let key_path_str = key_path.to_str()
        .ok_or_else(|| anyhow!("SSH key path contains invalid UTF-8"))?;

    // Get terminal environment from host to pass through
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
    let colorterm = std::env::var("COLORTERM").unwrap_or_else(|_| "truecolor".to_string());

    let status = std::process::Command::new("ssh")
        .args([
            "-o", &format!("ProxyCommand={}", proxy_cmd),
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            // Pass through terminal and locale settings for proper rendering
            "-o", &format!("SetEnv=TERM={} COLORTERM={} LANG=C.UTF-8 LC_ALL=C.UTF-8", term, colorterm),
            "-i", key_path_str,
            "-t",  // Force PTY allocation
            &format!("{}@localhost", user),
        ])
        .status()
        .context("Failed to spawn SSH")?;

    Ok(status)
}

/// Fallback to exec -it (doesn't support terminal resize)
async fn exec_shell_fallback(state: &ContainerState, container_id: &str) -> Result<()> {
    let status = if is_in_toolbox() {
        // In toolbox, use flatpak-spawn to reach host's podman
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "podman", "exec", "-it", container_id, "/bin/bash"])
            .status()
            .context("Failed to spawn shell via flatpak-spawn")?
    } else {
        // Direct podman/docker based on provider
        let runtime = match state.provider {
            devc_provider::ProviderType::Docker => "docker",
            devc_provider::ProviderType::Podman => "podman",
        };
        std::process::Command::new(runtime)
            .args(["exec", "-it", container_id, "/bin/bash"])
            .status()
            .context("Failed to spawn shell")?
    };

    if !status.success() {
        bail!("Shell exited with status: {}", status);
    }

    Ok(())
}

/// Check if running inside a toolbox/container that needs flatpak-spawn
fn is_in_toolbox() -> bool {
    std::path::Path::new("/run/.containerenv").exists()
}

/// Resize container terminal to match current terminal size
/// This is a lightweight command that doesn't need the full provider infrastructure
pub async fn resize(
    _manager: &ContainerManager,
    container: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<()> {
    // Load state directly to avoid provider connection issues
    let state_store = devc_core::StateStore::load()?;

    let state = match container {
        Some(ref name) => {
            state_store.find_by_name(name)
                .ok_or_else(|| anyhow!("Container '{}' not found", name))?
        }
        None => {
            let cwd = std::env::current_dir()?;
            state_store.find_by_workspace(&cwd)
                .ok_or_else(|| anyhow!("No container found for current directory"))?
        }
    };

    if state.status != DevcContainerStatus::Running {
        bail!("Container '{}' is not running", state.name);
    }

    let container_id = state.container_id.as_ref()
        .ok_or_else(|| anyhow!("Container has no container ID"))?;

    // Get terminal size - use args if provided, otherwise detect
    let (cols, rows) = match (cols, rows) {
        (Some(c), Some(r)) => (c, r),
        _ => crossterm::terminal::size().context("Could not determine terminal size. Use --cols and --rows to specify manually.")?,
    };

    // Build the resize command
    let resize_cmd = format!(
        "stty rows {} cols {} 2>/dev/null; pkill -SIGWINCH tmux 2>/dev/null; pkill -SIGWINCH nvim 2>/dev/null",
        rows, cols
    );

    // Determine how to reach the container runtime
    let status = if is_in_toolbox() {
        // Inside toolbox: use flatpak-spawn to reach host's podman
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "podman", "exec", container_id, "bash", "-c", &resize_cmd])
            .status()
            .context("Failed to exec via flatpak-spawn")?
    } else {
        // On host: try podman, fall back to docker
        std::process::Command::new("podman")
            .args(["exec", container_id, "bash", "-c", &resize_cmd])
            .status()
            .or_else(|_| {
                std::process::Command::new("docker")
                    .args(["exec", container_id, "bash", "-c", &resize_cmd])
                    .status()
            })
            .context("Failed to exec resize command")?
    };

    if status.success() {
        println!("Resized '{}' to {}x{}", state.name, cols, rows);
    } else {
        bail!("Failed to resize container");
    }

    Ok(())
}

/// Build a container
pub async fn build(manager: &ContainerManager, container: Option<String>, no_cache: bool) -> Result<()> {
    let state = match container {
        Some(name) => find_container(manager, &name).await?,
        None => {
            // Try current directory, init if not found
            match find_container_in_cwd(manager).await {
                Ok(state) => state,
                Err(_) => {
                    println!("No container found for current directory, initializing...");
                    manager.init(&std::env::current_dir()?).await?
                }
            }
        }
    };

    if no_cache {
        println!("Building '{}' (no cache)...", state.name);
    } else {
        println!("Building '{}'...", state.name);
    }

    let image_id = manager.build_with_options(&state.id, no_cache).await?;
    println!("Built image: {}", image_id);

    Ok(())
}

/// Start a container
pub async fn start(manager: &ContainerManager, container: &str) -> Result<()> {
    let state = find_container(manager, container).await?;

    if state.status == DevcContainerStatus::Running {
        println!("Container '{}' is already running", state.name);
        return Ok(());
    }

    if !state.can_start() {
        bail!(
            "Container '{}' cannot be started in {} state",
            state.name,
            state.status
        );
    }

    println!("Starting '{}'...", state.name);
    manager.start(&state.id).await?;
    println!("Started '{}'", state.name);

    Ok(())
}

/// Stop a container
pub async fn stop(manager: &ContainerManager, container: &str) -> Result<()> {
    let state = find_container(manager, container).await?;

    if state.status != DevcContainerStatus::Running {
        println!("Container '{}' is not running", state.name);
        return Ok(());
    }

    println!("Stopping '{}'...", state.name);
    manager.stop(&state.id).await?;
    println!("Stopped '{}'", state.name);

    Ok(())
}

/// Remove a container
pub async fn remove(manager: &ContainerManager, container: &str, force: bool) -> Result<()> {
    let state = find_container(manager, container).await?;

    if !force && !state.can_remove() {
        bail!(
            "Container '{}' cannot be removed in {} state (use --force)",
            state.name,
            state.status
        );
    }

    println!("Removing '{}'...", state.name);
    manager.remove(&state.id, force).await?;
    println!("Removed '{}'", state.name);

    Ok(())
}

/// List containers
pub async fn list(manager: &ContainerManager, discover: bool, sync: bool) -> Result<()> {
    if discover {
        return list_discovered(manager).await;
    }

    if sync {
        // Sync all managed containers first
        let containers = manager.list().await?;
        for container in &containers {
            let _ = manager.sync_status(&container.id).await;
        }
    }

    let containers = manager.list().await?;

    if containers.is_empty() {
        println!("No containers found.");
        println!("\nUse 'devc init' in a directory with devcontainer.json to add a container.");
        return Ok(());
    }

    // Column widths
    const NAME_WIDTH: usize = 26;
    const STATUS_WIDTH: usize = 12;
    const PROVIDER_WIDTH: usize = 10;

    // Header
    println!(
        "  {:<NAME_WIDTH$} {:<STATUS_WIDTH$} {:<PROVIDER_WIDTH$} WORKSPACE",
        "NAME", "STATUS", "PROVIDER"
    );
    println!("{}", "-".repeat(75));

    for container in containers {
        let status_symbol = match container.status {
            DevcContainerStatus::Running => "●",
            DevcContainerStatus::Stopped => "○",
            DevcContainerStatus::Building => "◐",
            DevcContainerStatus::Built => "◑",
            DevcContainerStatus::Created => "◔",
            DevcContainerStatus::Failed => "✗",
            DevcContainerStatus::Configured => "◯",
        };

        let workspace = container
            .workspace_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| container.workspace_path.to_string_lossy().to_string());

        // Pad name manually to handle Unicode symbol display width
        let name_padding = NAME_WIDTH.saturating_sub(container.name.len());
        let status_str = format!("{}", container.status);
        let status_padding = STATUS_WIDTH.saturating_sub(status_str.len());
        let provider_str = format!("{}", container.provider);
        let provider_padding = PROVIDER_WIDTH.saturating_sub(provider_str.len());

        println!(
            "{} {}{} {}{} {}{} {}",
            status_symbol,
            container.name, " ".repeat(name_padding),
            status_str, " ".repeat(status_padding),
            provider_str, " ".repeat(provider_padding),
            workspace
        );
    }

    Ok(())
}

/// List discovered devcontainers from all providers
async fn list_discovered(manager: &ContainerManager) -> Result<()> {
    use devc_provider::DevcontainerSource;

    let discovered = manager.discover().await?;

    if discovered.is_empty() {
        println!("No devcontainers found.");
        println!("\nTip: Create a devcontainer with VS Code or run 'devc init' to get started.");
        return Ok(());
    }

    // Column widths
    const NAME_WIDTH: usize = 26;
    const STATUS_WIDTH: usize = 12;
    const SOURCE_WIDTH: usize = 10;
    const MANAGED_WIDTH: usize = 7;

    // Header
    println!(
        "  {:<NAME_WIDTH$} {:<STATUS_WIDTH$} {:<SOURCE_WIDTH$} {:<MANAGED_WIDTH$} WORKSPACE",
        "NAME", "STATUS", "SOURCE", "MANAGED"
    );
    println!("{}", "-".repeat(85));

    for container in discovered {
        let status_symbol = match container.status {
            devc_provider::ContainerStatus::Running => "●",
            devc_provider::ContainerStatus::Exited => "○",
            devc_provider::ContainerStatus::Created => "◔",
            _ => "?",
        };

        let workspace = container.workspace_path.as_deref().unwrap_or("-");

        let managed_str = if container.managed { "✓" } else { "-" };
        let source_str = match container.source {
            DevcontainerSource::Devc => "devc",
            DevcontainerSource::VsCode => "vscode",
            DevcontainerSource::Other => "other",
        };

        // Pad fields
        let name_padding = NAME_WIDTH.saturating_sub(container.name.len());
        let status_str = format!("{}", container.status);
        let status_padding = STATUS_WIDTH.saturating_sub(status_str.len());
        let source_padding = SOURCE_WIDTH.saturating_sub(source_str.len());

        println!(
            "{} {}{} {}{} {}{} {:<MANAGED_WIDTH$} {}",
            status_symbol,
            container.name, " ".repeat(name_padding),
            status_str, " ".repeat(status_padding),
            source_str, " ".repeat(source_padding),
            managed_str,
            workspace
        );
    }

    println!();
    println!("Tip: Use 'devc adopt <name>' to import unmanaged containers into devc.");

    Ok(())
}

/// Initialize a new container from current directory
pub async fn init(manager: &ContainerManager) -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Check if devcontainer.json exists
    let devcontainer_path = cwd.join(".devcontainer/devcontainer.json");
    let devcontainer_alt = cwd.join(".devcontainer.json");

    if !devcontainer_path.exists() && !devcontainer_alt.exists() {
        bail!(
            "No devcontainer.json found in current directory.\n\
             Create .devcontainer/devcontainer.json first."
        );
    }

    // Check if already initialized
    let containers = manager.list().await?;
    if containers.iter().any(|c| c.workspace_path == cwd) {
        bail!("Container already initialized for this directory");
    }

    let state = manager.init(&cwd).await?;
    println!("Initialized container: {}", state.name);
    println!("\nNext steps:");
    println!("  devc build {}    # Build the container image", state.name);
    println!("  devc up {}       # Build, create, and start", state.name);
    println!("  devc shell {}      # Connect to the container", state.name);

    Ok(())
}

/// Build, create, and start a container
pub async fn up(manager: &ContainerManager, container: Option<String>) -> Result<()> {
    let state = match container {
        Some(name) => find_container(manager, &name).await?,
        None => {
            // Try current directory, init if not found
            match find_container_in_cwd(manager).await {
                Ok(state) => state,
                Err(_) => {
                    println!("No container found for current directory, initializing...");
                    manager.init(&std::env::current_dir()?).await?
                }
            }
        }
    };

    println!("Starting '{}'...", state.name);

    manager.up(&state.id).await?;

    println!("Container '{}' is running", state.name);
    println!("\nConnect with: devc shell {}", state.name);

    Ok(())
}

/// Stop and remove a container (but keep state so it can be recreated with `up`)
pub async fn down(manager: &ContainerManager, container: &str) -> Result<()> {
    let state = find_container(manager, container).await?;

    println!("Stopping '{}'...", state.name);
    manager.down(&state.id).await?;
    println!("Stopped '{}'", state.name);
    println!("\nRun 'devc up {}' to start it again.", state.name);

    Ok(())
}

/// Show or edit configuration
pub async fn config(edit: bool) -> Result<()> {
    let config_path = GlobalConfig::config_path()?;

    if edit {
        // Open in editor
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

        // Create config file with defaults if it doesn't exist
        if !config_path.exists() {
            let config = GlobalConfig::default();
            config.save()?;
            println!("Created default config at {:?}", config_path);
        }

        std::process::Command::new(&editor)
            .arg(&config_path)
            .status()
            .context(format!("Failed to open editor: {}", editor))?;
    } else {
        // Show config
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            println!("# Config file: {:?}\n", config_path);
            println!("{}", content);
        } else {
            println!("# Config file: {:?} (not created yet)\n", config_path);
            println!("# Default configuration:");
            let config = GlobalConfig::default();
            let content = toml::to_string_pretty(&config)?;
            println!("{}", content);
            println!("\n# Run 'devc config --edit' to create and edit the config file.");
        }
    }

    Ok(())
}

/// Find a container by name or ID
async fn find_container(manager: &ContainerManager, name_or_id: &str) -> Result<ContainerState> {
    // Try by name first
    if let Some(state) = manager.get_by_name(name_or_id).await? {
        return Ok(state);
    }

    // Try by ID
    if let Some(state) = manager.get(name_or_id).await? {
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
            matches
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Rebuild a container (destroy and rebuild, optionally with provider migration)
pub async fn rebuild(
    manager: &ContainerManager,
    container: &str,
    no_cache: bool,
    skip_confirm: bool,
) -> Result<()> {
    let state = find_container(manager, container).await?;

    // Check for provider change
    let current_provider = manager
        .provider_type()
        .ok_or_else(|| anyhow::anyhow!("Not connected to a container provider"))?;
    let provider_changed = state.provider != current_provider;

    // Show confirmation unless --yes
    if !skip_confirm {
        println!("Rebuild '{}'?", state.name);
        if provider_changed {
            println!(
                "  Warning: Provider will change: {} -> {}",
                state.provider, current_provider
            );
        }
        if no_cache {
            println!("  Warning: Cache disabled - full rebuild");
        }
        print!("Continue? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();

        if input != "y" && input != "yes" {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Execute rebuild
    println!("Rebuilding '{}'...", state.name);
    manager.rebuild(&state.id, no_cache).await?;
    println!("Rebuilt '{}' successfully", state.name);

    Ok(())
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

/// Adopt an existing devcontainer into devc management
pub async fn adopt(manager: &ContainerManager, container: Option<String>) -> Result<()> {
    use devc_provider::DevcontainerSource;

    // Discover all containers
    let discovered = manager.discover().await?;

    // Filter to unmanaged containers only
    let unmanaged: Vec<_> = discovered
        .iter()
        .filter(|c| !c.managed)
        .collect();

    if unmanaged.is_empty() {
        println!("No unmanaged devcontainers found to adopt.");
        println!("\nAll discovered devcontainers are already managed by devc.");
        return Ok(());
    }

    // Find the container to adopt
    let to_adopt = match container {
        Some(name_or_id) => {
            // Find by name or ID
            unmanaged
                .iter()
                .find(|c| c.name == name_or_id || c.id.0 == name_or_id || c.id.0.starts_with(&name_or_id))
                .ok_or_else(|| anyhow!(
                    "Container '{}' not found or already managed by devc.\nRun 'devc list --discover' to see available containers.",
                    name_or_id
                ))?
        }
        None => {
            // Interactive selection
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                bail!("No container specified. Use 'devc adopt <name>' or run interactively.");
            }

            // Show selection dialog
            println!("Select a container to adopt:\n");
            for (i, c) in unmanaged.iter().enumerate() {
                let source = match c.source {
                    DevcontainerSource::VsCode => "vscode",
                    DevcontainerSource::Other => "other",
                    DevcontainerSource::Devc => "devc",
                };
                let workspace = c.workspace_path.as_deref().unwrap_or("-");
                println!("  {}. {} ({}) - {}", i + 1, c.name, source, workspace);
            }
            println!("\nEnter number (or 'q' to cancel): ");

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input == "q" || input == "Q" {
                return Ok(());
            }

            let idx: usize = input.parse().context("Invalid selection")?;
            if idx == 0 || idx > unmanaged.len() {
                bail!("Invalid selection. Enter a number between 1 and {}", unmanaged.len());
            }

            unmanaged[idx - 1]
        }
    };

    println!("Adopting '{}'...", to_adopt.name);

    // Adopt the container
    let state = manager.adopt(&to_adopt.id.0, to_adopt.workspace_path.as_deref()).await?;

    println!("Adopted container: {}", state.name);
    println!("\nYou can now use devc commands with this container:");
    println!("  devc shell {}       # Connect to the container", state.name);
    println!("  devc stop {}      # Stop the container", state.name);
    println!("  devc rm {}        # Remove the container", state.name);

    Ok(())
}

