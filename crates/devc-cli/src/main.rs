//! devc - Dev Container Manager CLI

mod commands;
mod selector;

use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, Select};
use selector::{select_container, SelectionContext};
use devc_config::GlobalConfig;
use devc_core::ContainerManager;
use devc_provider::{create_default_provider, create_provider, detect_available_providers, ProviderType};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser)]
#[command(name = "devc")]
#[command(author, version, about = "Dev Container Manager", long_about = None)]
struct Cli {
    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Override default provider (docker or podman)
    #[arg(long, global = true, value_parser = ["docker", "podman"])]
    provider: Option<String>,

    /// Demo mode (show TUI without container runtime)
    #[arg(long)]
    demo: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a command in a container
    Run {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
        /// Command to run
        #[arg(trailing_var_arg = true)]
        cmd: Vec<String>,
    },

    /// Open an interactive shell in a container
    Ssh {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
    },

    /// Build a container
    Build {
        /// Container name or ID (optional, uses current directory if not specified)
        container: Option<String>,
        /// Don't use cache when building the image
        #[arg(long)]
        no_cache: bool,
    },

    /// Start a container
    Start {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
    },

    /// Stop a container
    Stop {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
    },

    /// Remove a container
    Rm {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
        /// Force removal even if running
        #[arg(short, long)]
        force: bool,
    },

    /// List containers
    List {
        /// Discover devcontainers from all providers (includes VS Code containers)
        #[arg(long)]
        discover: bool,
        /// Sync status with container runtimes
        #[arg(long)]
        sync: bool,
    },

    /// Initialize a new dev container from current directory
    Init,

    /// Build, create, and start a container
    Up {
        /// Container name or ID (optional, uses current directory if not specified)
        container: Option<String>,
    },

    /// Stop and remove a container
    Down {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
    },

    /// Resize container PTY (fixes nested tmux after zoom)
    Resize {
        /// Container name or ID (optional, uses current directory if not specified)
        container: Option<String>,
        /// Columns (width) - if not specified, uses current terminal
        #[arg(long, short = 'c')]
        cols: Option<u16>,
        /// Rows (height) - if not specified, uses current terminal
        #[arg(long, short = 'r')]
        rows: Option<u16>,
    },

    /// Show or edit global configuration
    Config {
        /// Open config in editor
        #[arg(short, long)]
        edit: bool,
    },

    /// Adopt an existing devcontainer into devc management
    Adopt {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
    },

    /// Rebuild a container (destroy and rebuild, optionally on new provider)
    Rebuild {
        /// Container name or ID (interactive selection if not specified)
        container: Option<String>,
        /// Force rebuild without using cache
        #[arg(long)]
        no_cache: bool,
        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();

    // Load global config
    let mut config = GlobalConfig::load().unwrap_or_default();

    // Handle config command separately (doesn't need provider)
    if let Some(Commands::Config { edit }) = &cli.command {
        commands::config(*edit).await?;
        return Ok(());
    }

    // First-run provider detection - only for CLI commands, not TUI
    // TUI handles provider selection itself with better UI
    if config.is_first_run() && !cli.demo && cli.provider.is_none() && cli.command.is_some() {
        if let Some(selected) = detect_and_select_provider(&config).await? {
            config.defaults.provider = match selected {
                ProviderType::Docker => "docker".to_string(),
                ProviderType::Podman => "podman".to_string(),
            };
            if let Err(e) = config.save() {
                eprintln!("Warning: Could not save provider selection: {}", e);
            } else {
                eprintln!("Provider '{}' saved to config", config.defaults.provider);
            }
        }
    }

    // Demo mode - run TUI without container runtime
    if cli.demo {
        devc_tui::run_demo().await?;
        return Ok(());
    }

    // Try to create a provider
    let provider_result = match cli.provider.as_deref() {
        Some("docker") => create_provider(ProviderType::Docker, &config).await,
        Some("podman") => create_provider(ProviderType::Podman, &config).await,
        _ => create_default_provider(&config).await,
    };

    // Handle TUI launch specially - allow starting in disconnected mode
    match cli.command {
        None => {
            // Launch TUI - create disconnected manager if provider fails
            let manager = match provider_result {
                Ok(provider) => ContainerManager::new(provider).await?,
                Err(e) => {
                    // Create disconnected manager for TUI
                    ContainerManager::disconnected(config, e.to_string())?
                }
            };
            devc_tui::run(manager).await?;
        }
        Some(cmd) => {
            // CLI commands require a working provider
            let provider = provider_result?;
            let manager = ContainerManager::new(provider).await?;

            // Get containers for selection (only when needed)
            let get_containers = || async { manager.list().await };

            match cmd {
                Commands::Run { container, cmd } => {
                    let name = match container {
                        Some(name) => name,
                        None => {
                            let containers = get_containers().await?;
                            select_container(&containers, SelectionContext::Running, "Select container to run command in:")?
                        }
                    };
                    commands::run(&manager, &name, cmd).await?;
                }
                Commands::Ssh { container } => {
                    let name = match container {
                        Some(name) => name,
                        None => {
                            let containers = get_containers().await?;
                            select_container(&containers, SelectionContext::Running, "Select container to connect to:")?
                        }
                    };
                    commands::ssh(&manager, &name).await?;
                }
                Commands::Build { container, no_cache } => {
                    commands::build(&manager, container, no_cache).await?;
                }
                Commands::Start { container } => {
                    let name = match container {
                        Some(name) => name,
                        None => {
                            let containers = get_containers().await?;
                            select_container(&containers, SelectionContext::Startable, "Select container to start:")?
                        }
                    };
                    commands::start(&manager, &name).await?;
                }
                Commands::Stop { container } => {
                    let name = match container {
                        Some(name) => name,
                        None => {
                            let containers = get_containers().await?;
                            select_container(&containers, SelectionContext::Running, "Select container to stop:")?
                        }
                    };
                    commands::stop(&manager, &name).await?;
                }
                Commands::Rm { container, force } => {
                    let name = match container {
                        Some(name) => name,
                        None => {
                            let containers = get_containers().await?;
                            select_container(&containers, SelectionContext::Any, "Select container to remove:")?
                        }
                    };
                    commands::remove(&manager, &name, force).await?;
                }
                Commands::List { discover, sync } => {
                    commands::list(&manager, discover, sync).await?;
                }
                Commands::Init => {
                    commands::init(&manager).await?;
                }
                Commands::Up { container } => {
                    let container = match container {
                        Some(name) => Some(name),
                        None => {
                            // up can work without selection (uses cwd), but offer selection if containers exist
                            let containers = get_containers().await?;
                            let uppable: Vec<_> = containers.iter()
                                .filter(|c| c.status != devc_core::DevcContainerStatus::Running)
                                .collect();
                            if !uppable.is_empty() && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                                // Offer selection but allow fallback to cwd behavior
                                match select_container(&containers, SelectionContext::Uppable, "Select container to bring up (or Esc for current directory):") {
                                    Ok(name) => Some(name),
                                    Err(_) => None, // Fall back to cwd behavior
                                }
                            } else {
                                None
                            }
                        }
                    };
                    commands::up(&manager, container).await?;
                }
                Commands::Down { container } => {
                    let name = match container {
                        Some(name) => name,
                        None => {
                            let containers = get_containers().await?;
                            select_container(&containers, SelectionContext::Any, "Select container to bring down:")?
                        }
                    };
                    commands::down(&manager, &name).await?;
                }
                Commands::Resize { container, cols, rows } => {
                    commands::resize(&manager, container, cols, rows).await?;
                }
                Commands::Config { .. } => unreachable!(), // Handled above
                Commands::Adopt { container } => {
                    commands::adopt(&manager, container).await?;
                }
                Commands::Rebuild { container, no_cache, yes } => {
                    let name = match container {
                        Some(name) => name,
                        None => {
                            let containers = get_containers().await?;
                            select_container(&containers, SelectionContext::Any, "Select container to rebuild:")?
                        }
                    };
                    commands::rebuild(&manager, &name, no_cache, yes).await?;
                }
            }
        }
    }

    Ok(())
}

/// Detect available providers and prompt user to select one if multiple are available
async fn detect_and_select_provider(config: &GlobalConfig) -> anyhow::Result<Option<ProviderType>> {
    eprintln!("First run detected - checking for container providers...");

    let available = detect_available_providers(config).await;

    let docker_available = available.iter()
        .find(|(t, _)| *t == ProviderType::Docker)
        .map(|(_, a)| *a)
        .unwrap_or(false);
    let podman_available = available.iter()
        .find(|(t, _)| *t == ProviderType::Podman)
        .map(|(_, a)| *a)
        .unwrap_or(false);

    match (docker_available, podman_available) {
        (false, false) => {
            eprintln!("No container providers detected.");
            eprintln!("Please install Docker or Podman and try again.");
            Ok(None)
        }
        (true, false) => {
            eprintln!("Auto-selected Docker (only available provider)");
            Ok(Some(ProviderType::Docker))
        }
        (false, true) => {
            eprintln!("Auto-selected Podman (only available provider)");
            Ok(Some(ProviderType::Podman))
        }
        (true, true) => {
            // Both available - prompt user to choose
            eprintln!("Both Docker and Podman are available.");

            // Check if we're in a terminal that supports interactive selection
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                let items = vec!["Docker (recommended)", "Podman"];
                let selection = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Select your preferred container provider")
                    .items(&items)
                    .default(0)
                    .interact()?;

                let provider = if selection == 0 {
                    ProviderType::Docker
                } else {
                    ProviderType::Podman
                };
                Ok(Some(provider))
            } else {
                // Non-interactive, default to Docker
                eprintln!("Non-interactive mode - defaulting to Docker");
                Ok(Some(ProviderType::Docker))
            }
        }
    }
}
