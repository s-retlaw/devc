//! Configuration parsing for devc
//!
//! This crate handles parsing of:
//! - Global configuration (`~/.config/devc/config.toml`)
//! - devcontainer.json files (VSCode compatible)

mod devcontainer;
mod error;
mod global;

pub use devcontainer::*;
pub use error::*;
pub use global::*;
