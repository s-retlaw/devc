//! Lifecycle commands: exec, shell, up, down, start, stop, build, rebuild

use anyhow::{anyhow, bail, Context, Result};
use devc_config::GlobalConfig;
use devc_core::{Container, ContainerManager, ContainerState, DevcContainerStatus};

use super::{find_container, find_container_in_cwd};

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
    let (program, prefix) = manager.runtime_args_for(&state)
        .map_err(|e| anyhow!("{}", e))?;

    let mut args: Vec<String> = prefix;
    args.push("exec".to_string());

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

    let status = std::process::Command::new(&program)
        .args(&args)
        .status()
        .context("Failed to spawn command")?;

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
            let (program, prefix) = manager.runtime_args_for(&state)
                .map_err(|e| anyhow!("{}", e))?;
            print_credential_status(manager, &state).await;
            return ssh_to_container(&state, &cmd, &program, &prefix).await;
        } else {
            bail!("Container '{}' is not running (status: {})", state.name, state.status);
        }
    }

    let (program, prefix) = manager.runtime_args_for(&state)
        .map_err(|e| anyhow!("{}", e))?;
    print_credential_status(manager, &state).await;
    ssh_to_container(&state, &cmd, &program, &prefix).await
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
async fn ssh_to_container(state: &ContainerState, cmd: &[String], program: &str, prefix: &[String]) -> Result<()> {
    let container_id = state.container_id.as_ref()
        .ok_or_else(|| anyhow!("Container has no container ID"))?;

    // Check if SSH is available for this container
    let ssh_available = state.metadata.get("ssh_available")
        .map(|v| v == "true")
        .unwrap_or(false);

    // Resolve effective user and workspace_folder from metadata or devcontainer.json
    let parsed = Container::from_config(&state.config_path).ok();
    let effective_user = state.metadata.get("remote_user").cloned()
        .or_else(|| parsed.as_ref().and_then(|c| c.devcontainer.effective_user().map(|s| s.to_string())));
    let workspace_folder = state.metadata.get("workspace_folder").cloned()
        .or_else(|| parsed.as_ref().and_then(|c| c.devcontainer.workspace_folder.clone()));

    if ssh_available {
        match ssh_via_dropbear(state, cmd, program, prefix, workspace_folder.as_deref()).await {
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

    // Fallback to exec -it
    exec_shell_fallback(program, prefix, container_id, cmd, effective_user.as_deref(), workspace_folder.as_deref()).await
}

/// Connect via SSH over stdio using dropbear in inetd mode
async fn ssh_via_dropbear(state: &ContainerState, cmd: &[String], program: &str, prefix: &[String], working_dir: Option<&str>) -> Result<std::process::ExitStatus> {
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

    // Build the proxy command using runtime args from the provider
    // Dropbear runs as daemon on 127.0.0.1:2222, we use socat to connect
    // Shell-quote container_id for safe interpolation into ProxyCommand
    let quoted_id = format!("'{}'", container_id.replace('\'', "'\\''"));
    let prefix_str = if prefix.is_empty() {
        String::new()
    } else {
        format!("{} ", prefix.join(" "))
    };
    let proxy_cmd = format!(
        "{} {}exec -i {} socat - TCP:127.0.0.1:2222",
        program, prefix_str, quoted_id
    );

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
    } else if let Some(wd) = working_dir {
        // For interactive shells, cd to the workspace folder
        args.push("--".to_string());
        args.push(format!("cd {} && exec $SHELL -l", shell_words::join(&[wd])));
    }

    let status = std::process::Command::new("ssh")
        .args(&args)
        .status()
        .context("Failed to spawn SSH")?;

    Ok(status)
}

/// Fallback to exec -it (doesn't support terminal resize)
async fn exec_shell_fallback(program: &str, prefix: &[String], container_id: &str, cmd: &[String], user: Option<&str>, working_dir: Option<&str>) -> Result<()> {
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

    let mut args: Vec<String> = prefix.to_vec();
    args.extend(["exec".to_string(), "-it".to_string()]);
    if let Some(u) = user {
        args.push("--user".to_string());
        args.push(u.to_string());
    }
    if let Some(wd) = working_dir {
        args.push("--workdir".to_string());
        args.push(wd.to_string());
    }
    args.push(container_id.to_string());
    args.extend(shell_args);

    let status = std::process::Command::new(program)
        .args(&args)
        .status()
        .context("Failed to spawn shell")?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// Resize container terminal to match current terminal size
pub async fn resize(
    manager: &ContainerManager,
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

    // Get runtime args from the container's provider
    let (program, prefix) = manager.runtime_args_for(state)
        .map_err(|e| anyhow!("{}", e))?;

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

    let mut args: Vec<String> = prefix;
    args.extend(["exec".to_string(), container_id.to_string(), "bash".to_string(), "-c".to_string(), resize_cmd]);

    let status = std::process::Command::new(&program)
        .args(&args)
        .status()
        .context("Failed to exec resize command")?;

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
