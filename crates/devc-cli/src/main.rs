//! devc - Dev Container Manager CLI

mod commands;

use clap::{Parser, Subcommand};
use devc_config::GlobalConfig;
use devc_core::ContainerManager;
use devc_provider::create_default_provider;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser)]
#[command(name = "devc")]
#[command(author, version, about = "Dev Container Manager", long_about = None)]
struct Cli {
    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

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
        /// Container name or ID
        container: String,
        /// Command to run
        #[arg(trailing_var_arg = true)]
        cmd: Vec<String>,
    },

    /// Open an interactive shell in a container
    Ssh {
        /// Container name or ID
        container: String,
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
        /// Container name or ID
        container: String,
    },

    /// Stop a container
    Stop {
        /// Container name or ID
        container: String,
    },

    /// Remove a container
    Rm {
        /// Container name or ID
        container: String,
        /// Force removal even if running
        #[arg(short, long)]
        force: bool,
    },

    /// List containers
    List {
        /// Show all containers (including stopped)
        #[arg(short, long)]
        all: bool,
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
        /// Container name or ID
        container: String,
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let config = GlobalConfig::load().unwrap_or_default();

    // Handle config command separately (doesn't need provider)
    if let Some(Commands::Config { edit }) = &cli.command {
        commands::config(*edit).await?;
        return Ok(());
    }

    // Demo mode - run TUI without container runtime
    if cli.demo {
        devc_tui::run_demo().await?;
        return Ok(());
    }

    // All other commands need a provider
    let provider = create_default_provider(&config).await?;
    let manager = ContainerManager::new(provider).await?;

    match cli.command {
        None => {
            // Launch TUI
            devc_tui::run(manager).await?;
        }
        Some(cmd) => {
            match cmd {
                Commands::Run { container, cmd } => {
                    commands::run(&manager, &container, cmd).await?;
                }
                Commands::Ssh { container } => {
                    commands::ssh(&manager, &container).await?;
                }
                Commands::Build { container, no_cache } => {
                    commands::build(&manager, container, no_cache).await?;
                }
                Commands::Start { container } => {
                    commands::start(&manager, &container).await?;
                }
                Commands::Stop { container } => {
                    commands::stop(&manager, &container).await?;
                }
                Commands::Rm { container, force } => {
                    commands::remove(&manager, &container, force).await?;
                }
                Commands::List { all } => {
                    commands::list(&manager, all).await?;
                }
                Commands::Init => {
                    commands::init(&manager).await?;
                }
                Commands::Up { container } => {
                    commands::up(&manager, container).await?;
                }
                Commands::Down { container } => {
                    commands::down(&manager, &container).await?;
                }
                Commands::Resize { container, cols, rows } => {
                    commands::resize(&manager, container, cols, rows).await?;
                }
                Commands::Config { .. } => unreachable!(), // Handled above
            }
        }
    }

    Ok(())
}
