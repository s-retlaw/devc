//! Management commands: init, remove, adopt, list, config, creds, agents

use anyhow::{anyhow, bail, Context, Result};
use devc_config::GlobalConfig;
use devc_core::{display_name_map, ContainerManager, DevcContainerStatus};

use super::{exec_check, find_container, find_container_in_cwd};

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
    let display_names = display_name_map(&containers);

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
            DevcContainerStatus::Available => "◌",
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
        let display_name = display_names
            .get(&container.id)
            .map(String::as_str)
            .unwrap_or(&container.name);
        let name_padding = NAME_WIDTH.saturating_sub(display_name.len());
        let status_str = format!("{}", container.status);
        let status_padding = STATUS_WIDTH.saturating_sub(status_str.len());
        let provider_str = format!("{}", container.provider);
        let provider_padding = PROVIDER_WIDTH.saturating_sub(provider_str.len());

        println!(
            "{} {}{} {}{} {}{} {}",
            status_symbol,
            display_name,
            " ".repeat(name_padding),
            status_str,
            " ".repeat(status_padding),
            provider_str,
            " ".repeat(provider_padding),
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

    // Header
    println!(
        "  {:<NAME_WIDTH$} {:<STATUS_WIDTH$} {:<SOURCE_WIDTH$} WORKSPACE",
        "NAME", "STATUS", "SOURCE"
    );
    println!("{}", "-".repeat(78));

    for container in discovered {
        let status_symbol = match container.status {
            devc_provider::ContainerStatus::Running => "●",
            devc_provider::ContainerStatus::Exited => "○",
            devc_provider::ContainerStatus::Created => "◔",
            _ => "?",
        };

        let workspace = container.workspace_path.as_deref().unwrap_or("-");

        let source_str = match container.source {
            DevcontainerSource::Devc => "devc",
            DevcontainerSource::VsCode => "vscode",
            DevcontainerSource::DevPod => "devpod",
            DevcontainerSource::Other => "other",
        };

        // Pad fields
        let name_padding = NAME_WIDTH.saturating_sub(container.name.len());
        let status_str = format!("{}", container.status);
        let status_padding = STATUS_WIDTH.saturating_sub(status_str.len());
        let source_padding = SOURCE_WIDTH.saturating_sub(source_str.len());

        println!(
            "{} {}{} {}{} {}{} {}",
            status_symbol,
            container.name,
            " ".repeat(name_padding),
            status_str,
            " ".repeat(status_padding),
            source_str,
            " ".repeat(source_padding),
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
    println!(
        "  devc shell {}      # Connect to the container",
        state.name
    );

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

/// Show credential forwarding diagnostics
pub async fn creds(manager: &ContainerManager, container: Option<String>) -> Result<()> {
    use devc_core::credentials::host;

    println!("Host Credential Status");
    println!("======================\n");

    // Docker config
    let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    if let Some(ref home) = home {
        let config_path = host::docker_config_path(home);
        if config_path.exists() {
            println!("Docker config: {} (found)", config_path.display());
        } else {
            println!("Docker config: {} (not found)", config_path.display());
        }
    } else {
        println!("Docker config: (could not determine home directory)");
    }

    // Docker credential helper detection
    let config = host::read_docker_cred_config().unwrap_or_default();
    let explicit_store = config.creds_store.as_deref().filter(|s| !s.is_empty());

    if let Some(store) = explicit_store {
        let binary = format!("docker-credential-{}", store);
        let found = host::which_exists(&binary);
        println!(
            "Configured credsStore: {} ({})",
            store,
            if found {
                "found in PATH"
            } else {
                "NOT found in PATH"
            }
        );
    } else {
        println!("Configured credsStore: (none)");
    }

    // Platform default detection
    let default_store = host::detect_default_creds_store();
    match (&explicit_store, &default_store) {
        (None, Some(store)) => {
            println!("Detected default helper: docker-credential-{}", store);
        }
        (None, None) => {
            println!("Detected default helper: (none found)");
        }
        _ => {} // explicit store takes precedence, already shown
    }

    // Resolve Docker credentials
    let effective_store = explicit_store.map(|s| s.to_string()).or(default_store);

    println!();
    if let Some(ref store) = effective_store {
        let registries = host::list_credential_helper_registries(store).await;
        if registries.is_empty() {
            println!(
                "Docker registries: (none found via docker-credential-{})",
                store
            );
        } else {
            println!(
                "Docker registries ({} via docker-credential-{}):",
                registries.len(),
                store
            );
            for reg in &registries {
                println!("  - {}", reg);
            }
        }
    }

    // Inline auths
    if !config.auths.is_empty() {
        let with_auth: Vec<_> = config
            .auths
            .iter()
            .filter(|(_, entry)| entry.auth.as_ref().is_some_and(|a| !a.is_empty()))
            .collect();
        if !with_auth.is_empty() {
            println!("Inline auths ({}):", with_auth.len());
            for (reg, _) in &with_auth {
                println!("  - {}", reg);
            }
        }
    }

    // Per-registry credHelpers
    if !config.cred_helpers.is_empty() {
        println!("Per-registry credHelpers ({}):", config.cred_helpers.len());
        for (reg, helper) in &config.cred_helpers {
            let binary = format!("docker-credential-{}", helper);
            let found = host::which_exists(&binary);
            println!(
                "  - {} -> {} ({})",
                reg,
                helper,
                if found { "found" } else { "NOT found" }
            );
        }
    }

    // Git credentials — discover hosts from workspace remotes
    println!("\nGit credentials:");
    let workspace_path = if let Some(ref container_name) = container {
        // If a container is specified, use its workspace path
        find_container(manager, container_name)
            .await
            .ok()
            .map(|s| s.workspace_path.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let git_hosts = host::discover_git_hosts(&workspace_path);
    if git_hosts.is_empty() {
        println!("  (no HTTPS git remotes found in workspace)");
    } else {
        for (protocol, host_name) in &git_hosts {
            match host::resolve_git_credential(protocol, host_name).await {
                Some(cred) => println!("  - {}: found (user: {})", host_name, cred.username),
                None => println!("  - {}: not configured", host_name),
            }
        }
    }

    // Container-side diagnostics (if container specified)
    if let Some(container_name) = container {
        println!("\nContainer Status ({})", container_name);
        println!("===============================\n");

        let state = find_container(manager, &container_name).await?;

        if state.status != DevcContainerStatus::Running {
            println!("Container is not running (status: {})", state.status);
            println!("Start the container to check credential forwarding status.");
            return Ok(());
        }

        let container_id = state
            .container_id
            .as_ref()
            .ok_or_else(|| anyhow!("Container has no container ID"))?;

        let provider = manager
            .provider_for_type(state.provider)
            .ok_or_else(|| anyhow!("{} provider not available", state.provider))?;
        let cid = devc_provider::ContainerId::new(container_id);

        let user = state
            .metadata
            .get("remote_user")
            .map(|s| s.as_str())
            .unwrap_or("root");

        // Check tmpfs mount
        let output = exec_check(
            provider,
            &cid,
            "test -d /run/devc-creds && echo yes || echo no",
            None,
        )
        .await;
        if output.as_deref().map(str::trim) == Some("yes") {
            println!("  tmpfs mount: /run/devc-creds (present)");
        } else {
            println!("  tmpfs mount: /run/devc-creds (MISSING)");
        }

        // Check cached config.json
        let output = exec_check(
            provider,
            &cid,
            "if [ -f /run/devc-creds/config.json ]; then cat /run/devc-creds/config.json; fi",
            None,
        )
        .await;
        if let Some(ref json) = output {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json) {
                let count = parsed
                    .get("auths")
                    .and_then(|a| a.as_object())
                    .map(|m| m.len())
                    .unwrap_or(0);
                println!("  cached credentials: {} registries", count);
            } else {
                println!("  cached credentials: present (could not parse)");
            }
        } else {
            println!("  cached credentials: (none)");
        }

        // Check credsStore in container's Docker config
        // Use $(whoami) instead of interpolating user to avoid shell injection
        let output = exec_check(
            provider, &cid,
            r#"if [ -z "$HOME" ]; then HOME=$(getent passwd "$(whoami)" 2>/dev/null | cut -d: -f6 || echo "/root"); fi; cat "$HOME/.docker/config.json" 2>/dev/null"#,
            Some(user),
        ).await;
        if let Some(ref json) = output {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json) {
                let store = parsed
                    .get("credsStore")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(not set)");
                println!("  credsStore: {}", store);
            } else {
                println!("  Docker config: present (could not parse)");
            }
        } else {
            println!("  Docker config: (not found)");
        }

        // Check helper scripts
        let output = exec_check(
            provider,
            &cid,
            "test -x /usr/local/bin/docker-credential-devc && echo yes || echo no",
            None,
        )
        .await;
        if output.as_deref().map(str::trim) == Some("yes") {
            println!("  docker-credential-devc: installed");
        } else {
            println!("  docker-credential-devc: NOT installed");
        }

        let output = exec_check(
            provider,
            &cid,
            "test -x /usr/local/bin/git-credential-devc && echo yes || echo no",
            None,
        )
        .await;
        if output.as_deref().map(str::trim) == Some("yes") {
            println!("  git-credential-devc: installed");
        } else {
            println!("  git-credential-devc: NOT installed");
        }

        // Check git-credentials file
        let output = exec_check(
            provider, &cid,
            "if [ -f /run/devc-creds/git-credentials ]; then wc -l < /run/devc-creds/git-credentials; else echo 0; fi",
            None,
        ).await;
        let count: usize = output
            .as_deref()
            .map(str::trim)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if count > 0 {
            println!("  cached git credentials: {} hosts", count);
        } else {
            println!("  cached git credentials: (none)");
        }
    } else {
        // No container specified - print guidance
        println!("\nTip: Run 'devc creds <container>' to also check container-side status.");
        if effective_store.is_none() && config.auths.is_empty() {
            println!("\nNo Docker credentials found on this host.");
            println!("Run 'docker login' to store credentials for Docker Hub.");
            println!("Run 'docker login <registry>' for other registries.");
        }
    }

    Ok(())
}

/// Show agent injection diagnostics.
pub async fn agents_doctor(manager: &ContainerManager, container: Option<String>) -> Result<()> {
    let config = manager.global_config();
    let availability = devc_core::agents::host_agent_availability(config);
    let enabled_results = devc_core::agents::doctor_enabled_agents(config);

    println!("Agent Doctor");
    println!("============\n");
    println!("Note: Agent auto-install requires Node/npm in the container image.");
    println!();

    if let Some(default_provider) = manager.provider_type() {
        println!("Default provider: {}", default_provider);
    }

    for item in &availability {
        let enabled = devc_core::agents::is_agent_enabled(config, item.agent, None);
        let state = if enabled { "enabled" } else { "disabled" };
        println!(
            "- {}: {}",
            item.agent,
            if item.available {
                format!("{state}, host config available")
            } else {
                format!(
                    "{state}, host config unavailable ({})",
                    item.reason.as_deref().unwrap_or("unknown reason")
                )
            }
        );
        if enabled {
            if !item.available {
                println!("  sync behavior: skipped (host config missing/unreadable)");
                continue;
            }
            if let Some(result) = enabled_results.iter().find(|r| r.agent == item.agent) {
                if result.warnings.is_empty() {
                    println!(
                        "  planned actions: copy host config, probe binary, install-if-missing (requires Node/npm)"
                    );
                } else {
                    for warning in &result.warnings {
                        println!("  warning: {}", warning);
                    }
                }
            } else {
                println!("  warning: enabled agent not found in diagnostics");
            }
        }
    }

    if let Some(name) = container {
        let state = find_container(manager, &name).await?;
        println!("\nContainer context: {}", state.name);
        println!("provider: {}", state.provider);
        println!("status: {}", state.status);
        if state.status != DevcContainerStatus::Running {
            println!("agent sync action: skipped (container not running)");
        } else {
            println!(
                "agent sync action: ready (run 'devc agents sync {}')",
                state.name
            );
        }
    }

    Ok(())
}

/// Force agent sync for a running container.
pub async fn agents_sync(manager: &ContainerManager, container: Option<String>) -> Result<()> {
    let state = match container {
        Some(name) => find_container(manager, &name).await?,
        None => find_container_in_cwd(manager).await?,
    };

    if state.status != DevcContainerStatus::Running {
        bail!(
            "Container '{}' is not running (status: {}). Start it first.",
            state.name,
            state.status
        );
    }

    println!("Syncing agents for '{}'...", state.name);
    let results = manager.setup_agents_for_container(&state.id).await?;

    if results.is_empty() {
        println!("No enabled agents to sync.");
        return Ok(());
    }

    let mut warning_count = 0usize;
    for result in results {
        if result.warnings.is_empty() {
            if result.installed {
                println!("- {}: ok (copied + installed)", result.agent);
            } else if result.copied {
                println!("- {}: ok (copied)", result.agent);
            } else {
                println!("- {}: skipped", result.agent);
            }
        } else {
            warning_count += result.warnings.len();
            println!("- {}: warning", result.agent);
            for warning in result.warnings {
                println!("  {}", warning);
            }
        }
    }

    if warning_count > 0 {
        println!(
            "\nAgent injection completed with {} warning(s). Run 'devc agents doctor' for details.",
            warning_count
        );
    } else {
        println!("\nAgent sync complete.");
    }

    Ok(())
}

/// Adopt an existing devcontainer into devc management
pub async fn adopt(manager: &ContainerManager, container: Option<String>) -> Result<()> {
    use devc_provider::DevcontainerSource;

    // Discover all containers
    let discovered = manager.discover().await?;

    // Filter to unmanaged containers only
    let unmanaged: Vec<_> = discovered
        .iter()
        .filter(|c| c.source != DevcontainerSource::Devc)
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
                    DevcontainerSource::DevPod => "devpod",
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
                bail!(
                    "Invalid selection. Enter a number between 1 and {}",
                    unmanaged.len()
                );
            }

            unmanaged[idx - 1]
        }
    };

    println!("Adopting '{}'...", to_adopt.name);

    // Adopt the container
    let state = manager
        .adopt(
            &to_adopt.id.0,
            to_adopt.workspace_path.as_deref(),
            to_adopt.source.clone(),
            to_adopt.provider,
        )
        .await?;

    println!("Adopted container: {}", state.name);
    println!("\nYou can now use devc commands with this container:");
    println!(
        "  devc shell {}       # Connect to the container",
        state.name
    );
    println!("  devc stop {}      # Stop the container", state.name);
    println!("  devc rm {}        # Remove the container", state.name);

    Ok(())
}
