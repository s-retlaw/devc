//! Credential forwarding for Docker and Git inside devcontainers
//!
//! Resolves credentials on the host, writes them to a tmpfs mount inside the
//! container, and installs chaining helper scripts that fall back to any
//! existing credential helper (e.g. VS Code's).

pub mod host;
pub mod inject;

pub use inject::setup_credentials;
