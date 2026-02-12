//! Core logic for devc container lifecycle management
//!
//! This crate provides:
//! - Container lifecycle management (build, start, stop, remove)
//! - State tracking for managed containers
//! - Dotfiles injection
//! - Command execution with proper PTY handling
//! - SSH setup for proper terminal resize support
//! - Enhanced builds with devc requirements (dropbear) injected

mod build;
mod container;
mod dotfiles;
mod error;
pub mod features;
mod manager;
mod ssh;
mod state;

pub use build::*;
pub use container::*;
pub use dotfiles::*;
pub use error::*;
pub use manager::*;
pub use ssh::*;
pub use state::*;

#[cfg(test)]
pub mod test_support;
