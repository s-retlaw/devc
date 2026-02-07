//! Build helpers for injecting devc requirements into container images
//!
//! This module handles injecting SSH support (dropbear) into container images
//! at build time, so container startup is fast.

use crate::Result;
use std::path::{Path, PathBuf};

/// Dropbear and socat installation script that detects the package manager
/// socat is needed to bridge pipes to sockets for SSH over stdio
/// Note: This script runs as USER root; the caller must restore the original USER afterward
const DROPBEAR_INSTALL_SCRIPT: &str = r#"
# Install dropbear and socat for devc SSH support
# Switch to root for package installation
USER root
RUN set -e; \
    if command -v apt-get >/dev/null 2>&1; then \
        apt-get update -qq && apt-get install -y -qq dropbear socat && rm -rf /var/lib/apt/lists/*; \
    elif command -v dnf >/dev/null 2>&1; then \
        dnf install -y -q dropbear socat && dnf clean all; \
    elif command -v yum >/dev/null 2>&1; then \
        yum install -y -q dropbear socat && yum clean all; \
    elif command -v apk >/dev/null 2>&1; then \
        apk add --no-cache dropbear socat; \
    elif command -v pacman >/dev/null 2>&1; then \
        pacman -Sy --noconfirm dropbear socat && pacman -Scc --noconfirm; \
    elif command -v zypper >/dev/null 2>&1; then \
        zypper -n install dropbear socat && zypper clean; \
    fi
"#;

/// Creates an enhanced Dockerfile that includes devc requirements (dropbear)
///
/// For image-based devcontainers, creates a new Dockerfile that extends the base image.
/// For Dockerfile-based, appends the installation to the existing Dockerfile.
pub struct EnhancedBuildContext {
    /// Temporary directory containing the enhanced build context
    temp_dir: tempfile::TempDir,
    /// Path to the enhanced Dockerfile
    dockerfile_path: PathBuf,
}

impl EnhancedBuildContext {
    /// Create an enhanced build context from a base image
    ///
    /// Creates a temporary Dockerfile that:
    /// 1. FROM <base_image>
    /// 2. Installs dropbear
    pub fn from_image(base_image: &str) -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;
        let dockerfile_path = temp_dir.path().join("Dockerfile");

        let dockerfile_content = format!(
            "FROM {}\n{}",
            base_image, DROPBEAR_INSTALL_SCRIPT
        );

        std::fs::write(&dockerfile_path, dockerfile_content)?;

        Ok(Self {
            temp_dir,
            dockerfile_path,
        })
    }

    /// Create an enhanced build context from an existing Dockerfile
    ///
    /// Copies the original build context and appends dropbear installation
    /// to the Dockerfile. If the original Dockerfile has a USER instruction,
    /// restores that user after the root-privileged installation.
    pub fn from_dockerfile(
        original_context: &Path,
        dockerfile_name: &str,
    ) -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;

        // Copy the entire build context to temp directory
        copy_dir_recursive(original_context, temp_dir.path())?;

        // Append dropbear installation to the Dockerfile
        let dockerfile_path = temp_dir.path().join(dockerfile_name);
        let original_content = std::fs::read_to_string(&dockerfile_path)?;

        // Find the last USER instruction to restore after dropbear install
        // This handles cases where the Dockerfile switches to a non-root user
        let last_user = original_content
            .lines()
            .rev()
            .find(|line| {
                let trimmed = line.trim();
                trimmed.to_uppercase().starts_with("USER ")
                    && !trimmed.starts_with('#')
            })
            .map(|line| line.trim().to_string());

        // Build the user restore instruction if needed
        let user_restore = last_user
            .map(|u| format!("\n# Restore original user\n{}", u))
            .unwrap_or_default();

        let enhanced_content = format!(
            "{}\n\n# Added by devc for SSH support{}{}",
            original_content, DROPBEAR_INSTALL_SCRIPT, user_restore
        );

        std::fs::write(&dockerfile_path, enhanced_content)?;

        Ok(Self {
            temp_dir,
            dockerfile_path,
        })
    }

    /// Get the path to the build context directory
    pub fn context_path(&self) -> &Path {
        self.temp_dir.path()
    }

    /// Get the Dockerfile name (relative to context)
    pub fn dockerfile_name(&self) -> &str {
        self.dockerfile_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Dockerfile")
    }
}

/// Recursively copy a directory
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== copy_dir_recursive tests ====================

    #[test]
    fn test_copy_dir_recursive_basic() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::write(src.path().join("file1.txt"), "hello").unwrap();
        std::fs::write(src.path().join("file2.txt"), "world").unwrap();

        copy_dir_recursive(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("file1.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("file2.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn test_copy_dir_recursive_nested() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::create_dir_all(src.path().join("sub/deep")).unwrap();
        std::fs::write(src.path().join("sub/deep/file.txt"), "nested").unwrap();

        copy_dir_recursive(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("sub/deep/file.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn test_copy_dir_recursive_empty_dir() {
        let src = tempfile::tempdir().unwrap();
        let dst_parent = tempfile::tempdir().unwrap();
        let dst = dst_parent.path().join("output");

        // Empty src directory
        copy_dir_recursive(src.path(), &dst).unwrap();
        assert!(dst.exists());
    }

    // ==================== from_dockerfile user restore tests ====================

    #[test]
    fn test_from_dockerfile_commented_user() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dockerfile_content = "FROM fedora:latest\n# USER vscode\nRUN echo hello\n";
        std::fs::write(temp_dir.path().join("Dockerfile"), dockerfile_content).unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile(temp_dir.path(), "Dockerfile").unwrap();
        let enhanced = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Commented USER should NOT trigger user restore
        assert!(!enhanced.contains("Restore original user"));
    }

    #[test]
    fn test_from_image_dockerfile_content() {
        let ctx = EnhancedBuildContext::from_image("alpine:3.18").unwrap();
        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        assert!(dockerfile.starts_with("FROM alpine:3.18"));
        assert!(dockerfile.contains("dropbear"));
        assert!(dockerfile.contains("socat"));
    }

    #[test]
    fn test_context_path_and_dockerfile_name() {
        let ctx = EnhancedBuildContext::from_image("ubuntu:22.04").unwrap();
        assert!(ctx.context_path().exists());
        assert_eq!(ctx.dockerfile_name(), "Dockerfile");
    }

    // ==================== Existing tests ====================

    #[test]
    fn test_from_image() {
        let ctx = EnhancedBuildContext::from_image("python:3.12").unwrap();
        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        assert!(dockerfile.starts_with("FROM python:3.12"));
        assert!(dockerfile.contains("dropbear"));
        assert!(dockerfile.contains("USER root"));
    }

    #[test]
    fn test_dropbear_script_valid() {
        // Ensure the script has proper Dockerfile syntax
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("USER root"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("RUN"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("apt-get"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("dnf"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("apk"));
    }

    #[test]
    fn test_from_dockerfile_restores_user() {
        // Create a temp directory with a Dockerfile that has a USER instruction
        let temp_dir = tempfile::tempdir().unwrap();
        let dockerfile_content = r#"FROM fedora:latest
RUN dnf install -y bash
USER vscode
SHELL ["/bin/bash", "-c"]
"#;
        std::fs::write(temp_dir.path().join("Dockerfile"), dockerfile_content).unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile(temp_dir.path(), "Dockerfile").unwrap();
        let enhanced = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should contain USER root for installation
        assert!(enhanced.contains("USER root"));
        // Should contain dropbear installation
        assert!(enhanced.contains("dropbear"));
        // Should restore the original user at the end
        // The last USER instruction should be "USER vscode"
        let lines: Vec<&str> = enhanced.lines().collect();
        let user_lines: Vec<&&str> = lines
            .iter()
            .filter(|l| l.trim().to_uppercase().starts_with("USER "))
            .collect();
        assert!(user_lines.len() >= 2, "Should have at least 2 USER instructions");
        assert!(
            user_lines.last().unwrap().contains("vscode"),
            "Last USER should restore to vscode"
        );
    }

    #[test]
    fn test_from_dockerfile_no_user() {
        // Create a temp directory with a Dockerfile without USER instruction
        let temp_dir = tempfile::tempdir().unwrap();
        let dockerfile_content = r#"FROM fedora:latest
RUN dnf install -y bash
"#;
        std::fs::write(temp_dir.path().join("Dockerfile"), dockerfile_content).unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile(temp_dir.path(), "Dockerfile").unwrap();
        let enhanced = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should contain USER root for installation
        assert!(enhanced.contains("USER root"));
        // Should contain dropbear installation
        assert!(enhanced.contains("dropbear"));
        // Should NOT have "Restore original user" since there was no USER instruction
        assert!(!enhanced.contains("Restore original user"));
    }
}
