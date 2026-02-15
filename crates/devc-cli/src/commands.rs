//! CLI command implementations

use anyhow::{anyhow, bail, Context, Result};
use devc_config::GlobalConfig;
use devc_core::{Container, ContainerManager, ContainerState, DevcContainerStatus};

/// Execute a command in a container (raw docker/podman exec)
pub async fn exec(manager: &ContainerManager, container: &str, cmd: Vec<String>) -> Result<()> {
    let state = find_container(manager, container).await?;

    if state.status != DevcContainerStatus::Running {
        bail!("Container '{}' is not running", state.name);
    }

    if cmd.is_empty() {
        bail!("No command specified");
    }

    let container_id = state
        .container_id
        .as_ref()
        .ok_or_else(|| anyhow!("Container has no container ID"))?;

    // Load config for remoteEnv/user/workdir (fallback if config is missing)
    let exec_config = match Container::from_config(&state.config_path) {
        Ok(container) => {
            let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
            container.exec_config(cmd, is_tty, true)
        }
        Err(_) => {
            let mut env = std::collections::HashMap::new();
            env.insert("TERM".to_string(), "xterm-256color".to_string());
            env.insert("COLORTERM".to_string(), "truecolor".to_string());
            env.insert("LANG".to_string(), "C.UTF-8".to_string());
            env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
            devc_provider::ExecConfig {
                cmd,
                env,
                working_dir: None,
                user: None,
                tty: std::io::IsTerminal::is_terminal(&std::io::stdin()),
                stdin: true,
                privileged: false,
            }
        }
    };

    // Build runtime args for direct spawn with inherited stdio
    let runtime = match state.provider {
        devc_provider::ProviderType::Docker => "docker",
        devc_provider::ProviderType::Podman => "podman",
    };

    let mut args = vec!["exec".to_string()];

    // TTY and stdin flags
    if exec_config.tty {
        args.push("-it".to_string());
    } else {
        args.push("-i".to_string());
    }

    if let Some(ref workdir) = exec_config.working_dir {
        args.push("--workdir".to_string());
        args.push(workdir.clone());
    }

    if let Some(ref user) = exec_config.user {
        args.push("--user".to_string());
        args.push(user.clone());
    }

    for (key, val) in &exec_config.env {
        args.push("-e".to_string());
        args.push(format!("{}={}", key, val));
    }

    args.push(container_id.clone());
    args.extend(exec_config.cmd);

    let status = if is_in_toolbox() {
        let mut fargs = vec!["--host".to_string(), "podman".to_string()];
        fargs.extend(args);
        std::process::Command::new("flatpak-spawn")
            .args(&fargs)
            .status()
            .context("Failed to spawn command via flatpak-spawn")?
    } else {
        std::process::Command::new(runtime)
            .args(&args)
            .status()
            .context("Failed to spawn command")?
    };

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// Open a shell in a container, optionally running a command
pub async fn shell(manager: &ContainerManager, container: &str, cmd: Vec<String>) -> Result<()> {
    let state = find_container(manager, container).await?;

    if state.status != DevcContainerStatus::Running {
        // Try to start it first
        if state.status == DevcContainerStatus::Stopped || state.status == DevcContainerStatus::Created {
            println!("Starting container '{}'...", state.name);
            manager.start(&state.id).await?;
            // Re-fetch state after starting
            let state = find_container(manager, container).await?;
            print_credential_status(manager, &state).await;
            return ssh_to_container(&state, &cmd).await;
        } else {
            bail!("Container '{}' is not running (status: {})", state.name, state.status);
        }
    }

    print_credential_status(manager, &state).await;
    ssh_to_container(&state, &cmd).await
}

/// Set up credential forwarding and print a one-line status
async fn print_credential_status(manager: &ContainerManager, state: &ContainerState) {
    match manager.setup_credentials_for_container(&state.id).await {
        Ok(status) if status.docker_registries > 0 || status.git_hosts > 0 => {
            eprintln!(
                "Forwarding credentials: {} Docker registries, {} Git hosts",
                status.docker_registries, status.git_hosts
            );
        }
        Ok(_) => {
            eprintln!("No host credentials found (run 'docker login' to store Docker credentials)");
        }
        Err(e) => {
            eprintln!("Warning: credential forwarding failed: {}", e);
        }
    }
}

/// Connect to container, preferring SSH over stdio when available
async fn ssh_to_container(state: &ContainerState, cmd: &[String]) -> Result<()> {
    let container_id = state.container_id.as_ref()
        .ok_or_else(|| anyhow!("Container has no container ID"))?;

    // Check if SSH is available for this container
    let ssh_available = state.metadata.get("ssh_available")
        .map(|v| v == "true")
        .unwrap_or(false);

    if ssh_available {
        match ssh_via_dropbear(state, cmd).await {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                // SSH exited with non-zero but we should still respect that
                std::process::exit(status.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("Warning: SSH connection failed ({}), falling back to exec", e);
                if cmd.is_empty() {
                    eprintln!("Note: Terminal resize will not work with exec fallback");
                }
            }
        }
    }

    // Fallback to podman/docker exec -it
    exec_shell_fallback(state, container_id, cmd).await
}

/// Connect via SSH over stdio using dropbear in inetd mode
async fn ssh_via_dropbear(state: &ContainerState, cmd: &[String]) -> Result<std::process::ExitStatus> {
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

    let mut args = vec![
        "-o".to_string(), format!("ProxyCommand={}", proxy_cmd),
        "-o".to_string(), "StrictHostKeyChecking=no".to_string(),
        "-o".to_string(), "UserKnownHostsFile=/dev/null".to_string(),
        "-o".to_string(), "LogLevel=ERROR".to_string(),
        // Pass through terminal and locale settings for proper rendering
        "-o".to_string(), format!("SetEnv=TERM={} COLORTERM={} LANG=C.UTF-8 LC_ALL=C.UTF-8", term, colorterm),
        "-i".to_string(), key_path_str.to_string(),
        "-t".to_string(),  // Force PTY allocation
        format!("{}@localhost", user),
    ];

    // Append command if provided — SSH runs it in a login shell and exits
    if !cmd.is_empty() {
        args.push("--".to_string());
        args.extend(cmd.iter().cloned());
    }

    let status = std::process::Command::new("ssh")
        .args(&args)
        .status()
        .context("Failed to spawn SSH")?;

    Ok(status)
}

/// Fallback to exec -it (doesn't support terminal resize)
async fn exec_shell_fallback(state: &ContainerState, container_id: &str, cmd: &[String]) -> Result<()> {
    // Build the shell command: interactive shell, or `bash -lc "cmd"` for commands
    let shell_args: Vec<String> = if cmd.is_empty() {
        vec!["/bin/bash".to_string()]
    } else {
        // Use bash -lc to get login shell environment (sources profile, sets PATH)
        vec![
            "/bin/bash".to_string(),
            "-lc".to_string(),
            shell_words::join(cmd),
        ]
    };

    let status = if is_in_toolbox() {
        let mut args = vec![
            "--host".to_string(), "podman".to_string(),
            "exec".to_string(), "-it".to_string(), container_id.to_string(),
        ];
        args.extend(shell_args);
        std::process::Command::new("flatpak-spawn")
            .args(&args)
            .status()
            .context("Failed to spawn shell via flatpak-spawn")?
    } else {
        let runtime = match state.provider {
            devc_provider::ProviderType::Docker => "docker",
            devc_provider::ProviderType::Podman => "podman",
        };
        let mut args = vec![
            "exec".to_string(), "-it".to_string(), container_id.to_string(),
        ];
        args.extend(shell_args);
        std::process::Command::new(runtime)
            .args(&args)
            .status()
            .context("Failed to spawn shell")?
    };

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
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
            state_store.get(name)
                .or_else(|| state_store.find_by_name(name))
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
            container.name, " ".repeat(name_padding),
            status_str, " ".repeat(status_padding),
            source_str, " ".repeat(source_padding),
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

/// Execute a shell script in a container and return stdout
async fn exec_check(
    provider: &dyn devc_provider::ContainerProvider,
    cid: &devc_provider::ContainerId,
    script: &str,
    user: Option<&str>,
) -> Option<String> {
    let result = provider.exec(cid, &devc_provider::ExecConfig {
        cmd: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        env: std::collections::HashMap::new(),
        working_dir: None,
        user: user.map(|s| s.to_string()),
        tty: false,
        stdin: false,
        privileged: false,
    }).await.ok()?;
    if result.exit_code != 0 || result.output.trim().is_empty() {
        return None;
    }
    Some(result.output)
}

/// Show credential forwarding diagnostics
pub async fn creds(manager: &ContainerManager, container: Option<String>) -> Result<()> {
    use devc_core::credentials::host;

    println!("Host Credential Status");
    println!("======================\n");

    // Docker config
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf());
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
            if found { "found in PATH" } else { "NOT found in PATH" }
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
    let effective_store = explicit_store
        .map(|s| s.to_string())
        .or(default_store);

    println!();
    if let Some(ref store) = effective_store {
        let registries = host::list_credential_helper_registries(store).await;
        if registries.is_empty() {
            println!("Docker registries: (none found via docker-credential-{})", store);
        } else {
            println!("Docker registries ({} via docker-credential-{}):", registries.len(), store);
            for reg in &registries {
                println!("  - {}", reg);
            }
        }
    }

    // Inline auths
    if !config.auths.is_empty() {
        let with_auth: Vec<_> = config.auths.iter()
            .filter(|(_, entry)| entry.auth.as_ref().map_or(false, |a| !a.is_empty()))
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
                reg, helper,
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

        let container_id = state.container_id.as_ref()
            .ok_or_else(|| anyhow!("Container has no container ID"))?;

        let provider = manager.provider()
            .ok_or_else(|| anyhow!("Not connected to a container provider"))?;
        let cid = devc_provider::ContainerId::new(container_id);

        let user = state.metadata.get("remote_user").map(|s| s.as_str()).unwrap_or("root");

        // Check tmpfs mount
        let output = exec_check(provider, &cid, "test -d /run/devc-creds && echo yes || echo no", None).await;
        if output.as_deref().map(str::trim) == Some("yes") {
            println!("  tmpfs mount: /run/devc-creds (present)");
        } else {
            println!("  tmpfs mount: /run/devc-creds (MISSING)");
        }

        // Check cached config.json
        let output = exec_check(
            provider, &cid,
            "if [ -f /run/devc-creds/config.json ]; then cat /run/devc-creds/config.json; fi",
            None,
        ).await;
        if let Some(ref json) = output {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json) {
                let count = parsed.get("auths")
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
                let store = parsed.get("credsStore")
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
        let output = exec_check(provider, &cid, "test -x /usr/local/bin/docker-credential-devc && echo yes || echo no", None).await;
        if output.as_deref().map(str::trim) == Some("yes") {
            println!("  docker-credential-devc: installed");
        } else {
            println!("  docker-credential-devc: NOT installed");
        }

        let output = exec_check(provider, &cid, "test -x /usr/local/bin/git-credential-devc && echo yes || echo no", None).await;
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
        let count: usize = output.as_deref().map(str::trim).and_then(|s| s.parse().ok()).unwrap_or(0);
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
    let state = manager.adopt(&to_adopt.id.0, to_adopt.workspace_path.as_deref(), to_adopt.source.clone(), to_adopt.provider).await?;

    println!("Adopted container: {}", state.name);
    println!("\nYou can now use devc commands with this container:");
    println!("  devc shell {}       # Connect to the container", state.name);
    println!("  devc stop {}      # Stop the container", state.name);
    println!("  devc rm {}        # Remove the container", state.name);

    Ok(())
}

