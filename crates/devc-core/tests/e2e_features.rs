//! End-to-end tests for devcontainer features.
//!
//! These tests verify that features can be downloaded from OCI registries,
//! HTTP tarball URLs, and local directories, and installed into container images.
//!
//! Integration tests require network access.
//! Full e2e tests require a container runtime (Docker or Podman).

use devc_core::features;
use devc_core::features::resolve::{parse_feature_ref, FeatureSource};
use devc_core::features::merge_feature_properties;
use devc_core::{Container, EnhancedBuildContext};
use devc_provider::{BuildConfig, CliProvider, ContainerProvider, CreateContainerConfig, ExecConfig};
use std::collections::HashMap;
use tempfile::TempDir;

/// Get a provider for testing (tries toolbox, podman, then docker)
async fn get_test_provider() -> Option<CliProvider> {
    if let Ok(p) = CliProvider::new_toolbox().await {
        return Some(p);
    }
    if let Ok(p) = CliProvider::new_podman().await {
        return Some(p);
    }
    if let Ok(p) = CliProvider::new_docker().await {
        return Some(p);
    }
    None
}

/// Create a temporary workspace with a devcontainer.json
fn create_test_workspace(devcontainer_json: &str) -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let devcontainer_dir = temp.path().join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir).expect("failed to create .devcontainer dir");
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        devcontainer_json,
    )
    .expect("failed to write devcontainer.json");
    temp
}

// ==========================================================================
// Integration test: OCI download (network only, no container runtime)
// ==========================================================================

#[tokio::test]
#[ignore] // Requires network access
async fn test_integration_oci_feature_download() {
    // Download a real feature from ghcr.io
    let cache_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();

    let source = parse_feature_ref("ghcr.io/devcontainers/features/git:1");
    match &source {
        FeatureSource::Oci {
            registry,
            namespace,
            name,
            tag,
        } => {
            assert_eq!(registry, "ghcr.io");
            assert_eq!(namespace, "devcontainers/features");
            assert_eq!(name, "git");
            assert_eq!(tag, "1");
        }
        _ => panic!("Expected OCI source"),
    }

    let result = features::download::download_feature(
        &source,
        config_dir.path(),
        cache_dir.path(),
        &None,
    )
    .await;

    let feature_dir = result.expect("download should succeed");

    // Verify the feature was extracted correctly
    assert!(
        feature_dir.join("install.sh").exists(),
        "install.sh should exist in downloaded feature"
    );
    assert!(
        feature_dir.join("devcontainer-feature.json").exists(),
        "devcontainer-feature.json should exist"
    );

    // Read and verify metadata
    let metadata = features::download::read_feature_metadata(&feature_dir);
    assert_eq!(metadata.id.as_deref(), Some("git"));

    // Verify caching: second download should be instant (returns cached path)
    let result2 = features::download::download_feature(
        &source,
        config_dir.path(),
        cache_dir.path(),
        &None,
    )
    .await;
    let feature_dir2 = result2.expect("cached download should succeed");
    assert_eq!(feature_dir, feature_dir2, "Should return same cached path");
}

// ==========================================================================
// Full e2e: Build container with multiple features, verify they installed
// ==========================================================================

#[tokio::test]
#[ignore] // Requires container runtime + network
async fn test_e2e_multiple_features_install() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Create workspace with multiple features.
    // We use mcr.microsoft.com/devcontainers/base:ubuntu which has apt-get.
    // Features: node (with specific version), git (defaults), and go.
    let workspace = create_test_workspace(
        r#"{
            "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
            "features": {
                "ghcr.io/devcontainers/features/node:1": {
                    "version": "20"
                },
                "ghcr.io/devcontainers/features/git:1": {},
                "ghcr.io/devcontainers/features/go:1": {
                    "version": "1.22"
                }
            },
            "remoteUser": "vscode"
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Resolve and prepare features
    let config_dir = container
        .config_path
        .parent()
        .unwrap()
        .to_path_buf();

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let progress = Some(progress_tx);

    let resolved = features::resolve_and_prepare_features(
        container.devcontainer.features.as_ref().unwrap(),
        &config_dir,
        &progress,
    )
    .await
    .expect("feature resolution should succeed");

    // Drain progress messages (just to verify they were sent)
    let mut progress_msgs = Vec::new();
    while let Ok(msg) = progress_rx.try_recv() {
        progress_msgs.push(msg);
    }
    assert!(
        !progress_msgs.is_empty(),
        "Should have received progress messages"
    );

    // Verify we got all 3 features resolved
    assert_eq!(resolved.len(), 3, "Should resolve 3 features");

    // All features should have install.sh
    for f in &resolved {
        assert!(
            f.dir.join("install.sh").exists(),
            "Feature {} should have install.sh",
            f.id
        );
    }

    // Build the enhanced image with features
    let remote_user = container
        .devcontainer
        .effective_user()
        .unwrap_or("root");
    let enhanced_ctx = EnhancedBuildContext::from_image_with_features(
        "mcr.microsoft.com/devcontainers/base:ubuntu",
        &resolved,
        false, // no SSH for this test
        remote_user,
    )
    .expect("enhanced build context should succeed");

    // Verify the Dockerfile looks right before building
    let dockerfile = std::fs::read_to_string(
        enhanced_ctx.context_path().join("Dockerfile"),
    )
    .unwrap();
    assert!(
        dockerfile.contains("FROM mcr.microsoft.com/devcontainers/base:ubuntu"),
        "Should have correct base image"
    );
    // Should have 3 feature COPY/RUN blocks
    let copy_count = dockerfile.matches("COPY feature-").count();
    assert_eq!(
        copy_count, 3,
        "Should have 3 feature COPY instructions, got {}",
        copy_count
    );
    assert!(
        dockerfile.contains("_REMOTE_USER=vscode"),
        "Should pass remote user to features"
    );

    let image_tag = format!("devc/test-features-e2e:latest");
    let build_config = BuildConfig {
        context: enhanced_ctx.context_path().to_path_buf(),
        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
        tag: image_tag.clone(),
        build_args: HashMap::new(),
        target: None,
        cache_from: Vec::new(),
        labels: HashMap::new(),
        no_cache: true,
        pull: true,
    };

    eprintln!("Building image with 3 features (this may take a while)...");
    let image_id = provider
        .build(&build_config)
        .await
        .expect("build should succeed");
    eprintln!("Build succeeded: {}", image_id.0);

    // Create and start a container from the built image
    let container_name = "devc-test-features-e2e";
    let _ = provider.remove_by_name(container_name).await;

    let create_config = CreateContainerConfig {
        name: Some(container_name.to_string()),
        image: image_tag.clone(),
        cmd: Some(vec![
            "bash".to_string(),
            "-c".to_string(),
            "sleep infinity".to_string(),
        ]),
        ..Default::default()
    };

    let cid = provider
        .create(&create_config)
        .await
        .expect("create should succeed");
    provider.start(&cid).await.expect("start should succeed");

    // Verify Node.js is installed with the right version
    let node_result = provider
        .exec(
            &cid,
            &ExecConfig {
                cmd: vec![
                    "node".into(), "--version".into(),
                ],
                ..Default::default()
            },
        )
        .await
        .expect("node exec should work");
    eprintln!("node --version: {}", node_result.output.trim());
    assert!(
        node_result.output.contains("v20"),
        "Node should be v20.x, got: {}",
        node_result.output.trim()
    );

    // Verify git is installed (git is typically on PATH without profile sourcing)
    let git_result = provider
        .exec(
            &cid,
            &ExecConfig {
                cmd: vec!["git".into(), "--version".into()],
                ..Default::default()
            },
        )
        .await
        .expect("git exec should work");
    eprintln!("git --version: {}", git_result.output.trim());
    assert!(
        git_result.output.contains("git version"),
        "git should be installed, got: {}",
        git_result.output.trim()
    );

    // Verify Go is installed with the right version
    let go_result = provider
        .exec(
            &cid,
            &ExecConfig {
                cmd: vec![
                    "go".into(), "version".into(),
                ],
                ..Default::default()
            },
        )
        .await
        .expect("go exec should work");
    eprintln!("go version: {}", go_result.output.trim());
    assert!(
        go_result.output.contains("go1.22"),
        "Go should be 1.22.x, got: {}",
        go_result.output.trim()
    );

    // Cleanup
    let _ = provider.remove(&cid, true).await;
    // Clean up the built image
    let _ = std::process::Command::new("docker")
        .args(["rmi", &image_tag])
        .output();
    eprintln!("E2E features test passed!");
}

// ==========================================================================
// E2E: Verify feature container properties (capAdd, securityOpt) are applied
// ==========================================================================

#[tokio::test]
#[ignore] // Requires container runtime + network
async fn test_e2e_feature_container_properties() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Build with the Go feature, which declares:
    //   "capAdd": ["SYS_PTRACE"], "securityOpt": ["seccomp=unconfined"]
    let workspace = create_test_workspace(
        r#"{
            "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
            "features": {
                "ghcr.io/devcontainers/features/go:1": {
                    "version": "latest"
                }
            },
            "remoteUser": "vscode"
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Resolve features
    let config_dir = container.config_path.parent().unwrap().to_path_buf();
    let resolved = features::resolve_and_prepare_features(
        container.devcontainer.features.as_ref().unwrap(),
        &config_dir,
        &None,
    )
    .await
    .expect("feature resolution should succeed");

    assert_eq!(resolved.len(), 1);

    // Merge feature properties — should pick up capAdd/securityOpt from go feature
    let feature_props = merge_feature_properties(&resolved);
    eprintln!("Merged feature properties: {:?}", feature_props);

    // The Go feature metadata should declare SYS_PTRACE
    assert!(
        feature_props.cap_add.contains(&"SYS_PTRACE".to_string()),
        "Go feature should request SYS_PTRACE capability, got: {:?}",
        feature_props.cap_add
    );

    // Build the enhanced image
    let remote_user = container.devcontainer.effective_user().unwrap_or("root");
    let enhanced_ctx = EnhancedBuildContext::from_image_with_features(
        "mcr.microsoft.com/devcontainers/base:ubuntu",
        &resolved,
        false,
        remote_user,
    )
    .expect("enhanced build context should succeed");

    let image_tag = "devc/test-feature-props-e2e:latest".to_string();
    let build_config = BuildConfig {
        context: enhanced_ctx.context_path().to_path_buf(),
        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
        tag: image_tag.clone(),
        build_args: HashMap::new(),
        target: None,
        cache_from: Vec::new(),
        labels: HashMap::new(),
        no_cache: true,
        pull: true,
    };

    eprintln!("Building image with Go feature...");
    provider
        .build(&build_config)
        .await
        .expect("build should succeed");

    // Create the container using create_config_with_features to apply feature props
    let create_config = container.create_config_with_features(&image_tag, Some(&feature_props));

    // Verify the create config has the expected properties
    assert!(
        create_config.cap_add.contains(&"SYS_PTRACE".to_string()),
        "Create config should include SYS_PTRACE, got: {:?}",
        create_config.cap_add
    );

    let container_name = "devc-test-feature-props-e2e";
    let _ = provider.remove_by_name(container_name).await;

    let mut config = create_config;
    config.name = Some(container_name.to_string());
    config.cmd = Some(vec![
        "bash".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);

    let cid = provider
        .create(&config)
        .await
        .expect("create should succeed");
    provider.start(&cid).await.expect("start should succeed");

    // Verify container has SYS_PTRACE capability via inspect
    // Use the provider's runtime command to inspect
    let runtime = if std::process::Command::new("podman")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "podman"
    } else {
        "docker"
    };

    let inspect_output = std::process::Command::new(runtime)
        .args(["inspect", "--format", "{{json .HostConfig.CapAdd}}", &cid.0])
        .output()
        .expect("inspect should work");
    let cap_add_json = String::from_utf8_lossy(&inspect_output.stdout);
    eprintln!("Container CapAdd: {}", cap_add_json.trim());
    assert!(
        cap_add_json.contains("SYS_PTRACE"),
        "Container should have SYS_PTRACE capability, inspect output: {}",
        cap_add_json.trim()
    );

    // Cleanup
    let _ = provider.remove(&cid, true).await;
    let _ = std::process::Command::new(runtime)
        .args(["rmi", &image_tag])
        .output();
    eprintln!("E2E feature container properties test passed!");
}

// ==========================================================================
// E2E: Verify feature mounts are applied to the container
// ==========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_feature_mounts() {
    use devc_config::Mount;

    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Create a workspace with a local feature that declares a mount
    let workspace = TempDir::new().expect("temp dir");
    let dc_dir = workspace.path().join(".devcontainer");
    std::fs::create_dir_all(&dc_dir).unwrap();

    // Create local feature with a mount declaration
    let feature_dir = dc_dir.join("my-mount-feature");
    std::fs::create_dir_all(&feature_dir).unwrap();
    std::fs::write(
        feature_dir.join("devcontainer-feature.json"),
        r#"{
            "id": "my-mount-feature",
            "version": "1.0.0",
            "mounts": [
                {
                    "type": "volume",
                    "source": "devc-test-feature-mount-vol",
                    "target": "/feature-mount-data"
                }
            ]
        }"#,
    )
    .unwrap();
    std::fs::write(
        feature_dir.join("install.sh"),
        "#!/bin/bash\necho 'mount feature installed'\n",
    )
    .unwrap();

    // Create devcontainer.json with the local feature
    std::fs::write(
        dc_dir.join("devcontainer.json"),
        r#"{
            "image": "ubuntu:22.04",
            "features": {
                "./my-mount-feature": true
            }
        }"#,
    )
    .unwrap();

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Resolve features
    let config_dir = container.config_path.parent().unwrap().to_path_buf();
    let resolved = features::resolve_and_prepare_features(
        container.devcontainer.features.as_ref().unwrap(),
        &config_dir,
        &None,
    )
    .await
    .expect("feature resolution should succeed");

    assert_eq!(resolved.len(), 1);

    // Merge feature properties — should pick up the mount
    let feature_props = merge_feature_properties(&resolved);
    eprintln!("Merged feature properties mounts: {:?}", feature_props.mounts);
    assert_eq!(feature_props.mounts.len(), 1);
    match &feature_props.mounts[0] {
        Mount::Object(obj) => {
            assert_eq!(obj.target, "/feature-mount-data");
        }
        _ => panic!("Expected object mount from feature metadata"),
    }

    // Build the enhanced image
    let remote_user = container.devcontainer.effective_user().unwrap_or("root");
    let enhanced_ctx = EnhancedBuildContext::from_image_with_features(
        "ubuntu:22.04",
        &resolved,
        false,
        remote_user,
    )
    .expect("enhanced build context should succeed");

    let image_tag = "devc/test-feature-mounts-e2e:latest".to_string();
    let build_config = BuildConfig {
        context: enhanced_ctx.context_path().to_path_buf(),
        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
        tag: image_tag.clone(),
        build_args: HashMap::new(),
        target: None,
        cache_from: Vec::new(),
        labels: HashMap::new(),
        no_cache: false,
        pull: false,
    };

    eprintln!("Building image with local mount feature...");
    provider
        .build(&build_config)
        .await
        .expect("build should succeed");

    // Create the container with feature mounts applied
    let create_config = container.create_config_with_features(&image_tag, Some(&feature_props));

    // Verify the create config includes the feature mount
    let has_feature_mount = create_config
        .mounts
        .iter()
        .any(|m| m.target == "/feature-mount-data");
    assert!(
        has_feature_mount,
        "Create config should include feature mount at /feature-mount-data, mounts: {:?}",
        create_config.mounts
    );

    let container_name = "devc-test-feature-mounts-e2e";
    let _ = provider.remove_by_name(container_name).await;

    let mut config = create_config;
    config.name = Some(container_name.to_string());
    config.cmd = Some(vec![
        "bash".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);

    let cid = provider
        .create(&config)
        .await
        .expect("create should succeed");
    provider.start(&cid).await.expect("start should succeed");

    // Verify the mount is present via inspect
    let runtime = if std::process::Command::new("podman")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "podman"
    } else {
        "docker"
    };

    let inspect_output = std::process::Command::new(runtime)
        .args(["inspect", "--format", "{{json .Mounts}}", &cid.0])
        .output()
        .expect("inspect should work");
    let mounts_json = String::from_utf8_lossy(&inspect_output.stdout);
    eprintln!("Container Mounts: {}", mounts_json.trim());
    assert!(
        mounts_json.contains("/feature-mount-data"),
        "Container should have /feature-mount-data mount, inspect output: {}",
        mounts_json.trim()
    );

    // Cleanup
    let _ = provider.remove(&cid, true).await;
    let _ = std::process::Command::new(runtime)
        .args(["rmi", &image_tag])
        .output();
    // Clean up the test volume
    let _ = std::process::Command::new(runtime)
        .args(["volume", "rm", "devc-test-feature-mount-vol"])
        .output();
    eprintln!("E2E feature mounts test passed!");
}

// ==========================================================================
// E2E: Verify feature lifecycle commands run before devcontainer.json commands
// ==========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_feature_lifecycle_commands() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Create a workspace with a local feature that declares a postCreateCommand
    let workspace = TempDir::new().expect("temp dir");
    let dc_dir = workspace.path().join(".devcontainer");
    std::fs::create_dir_all(&dc_dir).unwrap();

    // Create local feature with a postCreateCommand that writes a marker file
    let feature_dir = dc_dir.join("lifecycle-feature");
    std::fs::create_dir_all(&feature_dir).unwrap();
    std::fs::write(
        feature_dir.join("devcontainer-feature.json"),
        r#"{
            "id": "lifecycle-feature",
            "version": "1.0.0",
            "postCreateCommand": "echo feature-post-create > /tmp/feature-lifecycle-marker"
        }"#,
    )
    .unwrap();
    std::fs::write(
        feature_dir.join("install.sh"),
        "#!/bin/bash\necho 'lifecycle feature installed'\n",
    )
    .unwrap();

    // devcontainer.json with the local feature AND its own postCreateCommand
    std::fs::write(
        dc_dir.join("devcontainer.json"),
        r#"{
            "image": "ubuntu:22.04",
            "features": {
                "./lifecycle-feature": true
            },
            "postCreateCommand": "echo devcontainer-post-create > /tmp/devcontainer-lifecycle-marker"
        }"#,
    )
    .unwrap();

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Resolve features
    let config_dir = container.config_path.parent().unwrap().to_path_buf();
    let resolved = features::resolve_and_prepare_features(
        container.devcontainer.features.as_ref().unwrap(),
        &config_dir,
        &None,
    )
    .await
    .expect("feature resolution should succeed");

    assert_eq!(resolved.len(), 1);

    // Merge feature properties — should pick up postCreateCommand
    let feature_props = merge_feature_properties(&resolved);
    assert_eq!(
        feature_props.post_create_commands.len(),
        1,
        "Should have 1 feature postCreateCommand"
    );

    // Build the enhanced image
    let remote_user = container.devcontainer.effective_user().unwrap_or("root");
    let enhanced_ctx = EnhancedBuildContext::from_image_with_features(
        "ubuntu:22.04",
        &resolved,
        false,
        remote_user,
    )
    .expect("enhanced build context should succeed");

    let image_tag = "devc/test-feature-lifecycle-e2e:latest".to_string();
    let build_config = BuildConfig {
        context: enhanced_ctx.context_path().to_path_buf(),
        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
        tag: image_tag.clone(),
        build_args: HashMap::new(),
        target: None,
        cache_from: Vec::new(),
        labels: HashMap::new(),
        no_cache: false,
        pull: false,
    };

    eprintln!("Building image with lifecycle feature...");
    provider
        .build(&build_config)
        .await
        .expect("build should succeed");

    // Create the container
    let create_config = container.create_config_with_features(&image_tag, Some(&feature_props));
    let container_name = "devc-test-feature-lifecycle-e2e";
    let _ = provider.remove_by_name(container_name).await;

    let mut config = create_config;
    config.name = Some(container_name.to_string());
    config.cmd = Some(vec![
        "bash".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);

    let cid = provider
        .create(&config)
        .await
        .expect("create should succeed");
    provider.start(&cid).await.expect("start should succeed");

    // Simulate what the manager does: run feature lifecycle commands, then devcontainer.json
    use devc_core::run_feature_lifecycle_commands;
    use devc_core::run_lifecycle_command_with_env;

    // Run feature postCreateCommands
    run_feature_lifecycle_commands(
        &provider,
        &cid,
        &feature_props.post_create_commands,
        None,
        None,
        None,
    )
    .await
    .expect("feature postCreateCommand should succeed");

    // Run devcontainer.json postCreateCommand
    if let Some(ref cmd) = container.devcontainer.post_create_command {
        run_lifecycle_command_with_env(&provider, &cid, cmd, None, None, None)
            .await
            .expect("devcontainer postCreateCommand should succeed");
    }

    // Verify the feature marker file exists
    let feature_marker = provider
        .exec(
            &cid,
            &ExecConfig {
                cmd: vec!["cat".into(), "/tmp/feature-lifecycle-marker".into()],
                ..Default::default()
            },
        )
        .await
        .expect("should read feature marker");
    assert!(
        feature_marker.output.contains("feature-post-create"),
        "Feature postCreateCommand should have run, got: {}",
        feature_marker.output.trim()
    );

    // Verify the devcontainer.json marker file exists
    let dc_marker = provider
        .exec(
            &cid,
            &ExecConfig {
                cmd: vec!["cat".into(), "/tmp/devcontainer-lifecycle-marker".into()],
                ..Default::default()
            },
        )
        .await
        .expect("should read devcontainer marker");
    assert!(
        dc_marker.output.contains("devcontainer-post-create"),
        "devcontainer.json postCreateCommand should have run, got: {}",
        dc_marker.output.trim()
    );

    // Cleanup
    let _ = provider.remove(&cid, true).await;
    let runtime = if std::process::Command::new("podman")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "podman"
    } else {
        "docker"
    };
    let _ = std::process::Command::new(runtime)
        .args(["rmi", &image_tag])
        .output();
    eprintln!("E2E feature lifecycle commands test passed!");
}

// ==========================================================================
// E2E: Verify features install via exec in compose containers
// ==========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_compose_feature_install() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Create a workspace with docker-compose.yml + devcontainer.json + local feature
    let workspace = TempDir::new().expect("temp dir");
    let dc_dir = workspace.path().join(".devcontainer");
    std::fs::create_dir_all(&dc_dir).unwrap();

    // docker-compose.yml: simple alpine service
    std::fs::write(
        dc_dir.join("docker-compose.yml"),
        r#"
services:
  app:
    image: alpine:latest
    command: ["sleep", "infinity"]
"#,
    )
    .unwrap();

    // Local feature that writes a marker file
    let feature_dir = dc_dir.join("test-compose-feature");
    std::fs::create_dir_all(&feature_dir).unwrap();
    std::fs::write(
        feature_dir.join("devcontainer-feature.json"),
        r#"{
            "id": "test-compose-feature",
            "version": "1.0.0",
            "containerEnv": {
                "MY_FEATURE_VAR": "hello-from-feature"
            }
        }"#,
    )
    .unwrap();
    std::fs::write(
        feature_dir.join("install.sh"),
        "#!/bin/sh\necho \"feature-was-installed\" > /tmp/compose-feature-marker\n",
    )
    .unwrap();

    // devcontainer.json with compose + feature
    std::fs::write(
        dc_dir.join("devcontainer.json"),
        r#"{
            "dockerComposeFile": "docker-compose.yml",
            "service": "app",
            "features": {
                "./test-compose-feature": true
            }
        }"#,
    )
    .unwrap();

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");
    assert!(container.is_compose(), "should be compose project");

    // Resolve features
    let config_dir = container.config_path.parent().unwrap().to_path_buf();
    let resolved = features::resolve_and_prepare_features(
        container.devcontainer.features.as_ref().unwrap(),
        &config_dir,
        &None,
    )
    .await
    .expect("feature resolution should succeed");
    assert_eq!(resolved.len(), 1);

    // Check feature properties (should have containerEnv but no container props)
    let feature_props = merge_feature_properties(&resolved);
    assert!(
        !feature_props.has_container_properties(),
        "Local test feature should not declare container properties"
    );

    // Start compose services
    let compose_files = container.compose_files().expect("should have compose files");
    let compose_file_strs: Vec<&str> = compose_files.iter().map(|p| p.to_str().unwrap()).collect();
    let project_name = container.compose_project_name();
    let project_dir = container.config_path.parent().unwrap();

    // Clean up any previous run
    let _ = provider
        .compose_down(&compose_file_strs, &project_name, project_dir)
        .await;

    provider
        .compose_up(&compose_file_strs, &project_name, project_dir, None)
        .await
        .expect("compose_up should succeed");

    // Find the app service container
    let services = provider
        .compose_ps(&compose_file_strs, &project_name, project_dir)
        .await
        .expect("compose_ps should succeed");

    let app_service = services
        .iter()
        .find(|s| s.service_name.contains("app"))
        .expect("should find app service");
    let cid = &app_service.container_id;

    // Install features via exec
    features::install::install_features_via_exec(&provider, cid, &resolved, "root", None)
        .await
        .expect("feature install via exec should succeed");

    // Verify the marker file was created by install.sh
    let marker_result = provider
        .exec(
            cid,
            &ExecConfig {
                cmd: vec!["cat".into(), "/tmp/compose-feature-marker".into()],
                ..Default::default()
            },
        )
        .await
        .expect("should read marker file");
    assert!(
        marker_result.output.contains("feature-was-installed"),
        "Feature install.sh should have created marker file, got: {}",
        marker_result.output.trim()
    );

    // Verify containerEnv was written to profile script
    let env_result = provider
        .exec(
            cid,
            &ExecConfig {
                cmd: vec!["cat".into(), "/etc/profile.d/devc-features.sh".into()],
                ..Default::default()
            },
        )
        .await
        .expect("should read profile script");
    assert!(
        env_result.output.contains("MY_FEATURE_VAR"),
        "Profile script should contain MY_FEATURE_VAR, got: {}",
        env_result.output.trim()
    );
    assert!(
        env_result.output.contains("hello-from-feature"),
        "Profile script should contain feature env value, got: {}",
        env_result.output.trim()
    );

    // Cleanup
    let _ = provider
        .compose_down(&compose_file_strs, &project_name, project_dir)
        .await;
    eprintln!("E2E compose feature install test passed!");
}

// ==========================================================================
// E2E: Docker-in-Docker feature install (OCI download + privileged mode)
// ==========================================================================

#[tokio::test]
#[ignore] // Requires container runtime + network
async fn test_e2e_docker_in_docker_feature_install() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Create workspace with docker-in-docker feature + privileged mode
    let workspace = create_test_workspace(
        r#"{
            "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
            "features": {
                "ghcr.io/devcontainers/features/docker-in-docker:2": {}
            },
            "privileged": true,
            "remoteUser": "vscode"
        }"#,
    );

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Resolve and prepare features — exercises full OCI auth + manifest + blob download
    let config_dir = container
        .config_path
        .parent()
        .unwrap()
        .to_path_buf();

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let progress = Some(progress_tx);

    let resolved = features::resolve_and_prepare_features(
        container.devcontainer.features.as_ref().unwrap(),
        &config_dir,
        &progress,
    )
    .await
    .expect("feature resolution should succeed");

    // Drain progress messages
    let mut progress_msgs = Vec::new();
    while let Ok(msg) = progress_rx.try_recv() {
        progress_msgs.push(msg);
    }
    assert!(
        !progress_msgs.is_empty(),
        "Should have received progress messages"
    );

    assert_eq!(resolved.len(), 1, "Should resolve 1 feature");
    assert!(
        resolved[0].dir.join("install.sh").exists(),
        "docker-in-docker feature should have install.sh"
    );

    // Verify privileged comes through from feature metadata
    let feature_props = merge_feature_properties(&resolved);
    eprintln!("Merged feature properties: {:?}", feature_props);
    // The docker-in-docker feature declares privileged: true in its metadata
    assert!(
        feature_props.privileged,
        "docker-in-docker feature should request privileged mode, got: {:?}",
        feature_props
    );

    // Build the enhanced image with the feature
    let remote_user = container
        .devcontainer
        .effective_user()
        .unwrap_or("root");
    let enhanced_ctx = EnhancedBuildContext::from_image_with_features(
        "mcr.microsoft.com/devcontainers/base:ubuntu",
        &resolved,
        false,
        remote_user,
    )
    .expect("enhanced build context should succeed");

    let image_tag = "devc/test-dind-e2e:latest".to_string();
    let build_config = BuildConfig {
        context: enhanced_ctx.context_path().to_path_buf(),
        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
        tag: image_tag.clone(),
        build_args: HashMap::new(),
        target: None,
        cache_from: Vec::new(),
        labels: HashMap::new(),
        no_cache: true,
        pull: true,
    };

    eprintln!("Building image with docker-in-docker feature (this may take a while)...");
    let image_id = provider
        .build(&build_config)
        .await
        .expect("build should succeed");
    eprintln!("Build succeeded: {}", image_id.0);

    // Create and start a container with privileged mode from feature props
    let container_name = "devc-test-dind-e2e";
    let _ = provider.remove_by_name(container_name).await;

    let mut create_config =
        container.create_config_with_features(&image_tag, Some(&feature_props));
    create_config.name = Some(container_name.to_string());
    create_config.cmd = Some(vec![
        "bash".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);

    // Verify privileged is set on the create config
    assert!(
        create_config.privileged,
        "Container create config should have privileged=true"
    );

    let cid = provider
        .create(&create_config)
        .await
        .expect("create should succeed");
    provider.start(&cid).await.expect("start should succeed");

    // Verify docker CLI is installed inside the container
    let docker_result = provider
        .exec(
            &cid,
            &ExecConfig {
                cmd: vec!["docker".into(), "--version".into()],
                ..Default::default()
            },
        )
        .await
        .expect("docker exec should work");
    eprintln!("docker --version: {}", docker_result.output.trim());
    assert!(
        docker_result.output.contains("Docker version"),
        "docker-in-docker should install docker CLI, got: {}",
        docker_result.output.trim()
    );

    // Cleanup
    let _ = provider.remove(&cid, true).await;
    let runtime = if std::process::Command::new("podman")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "podman"
    } else {
        "docker"
    };
    let _ = std::process::Command::new(runtime)
        .args(["rmi", &image_tag])
        .output();
    eprintln!("E2E docker-in-docker feature install test passed!");
}

// ==========================================================================
// Helper: create a tar.gz in memory containing install.sh + metadata
// ==========================================================================

fn create_test_feature_tarball(feature_id: &str, install_script: &str) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let buf = Vec::new();
    let encoder = GzEncoder::new(buf, Compression::default());
    let mut archive = tar::Builder::new(encoder);

    let install_bytes = install_script.as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_size(install_bytes.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "install.sh", install_bytes)
        .unwrap();

    let metadata = format!(
        r#"{{"id": "{}", "version": "1.0.0"}}"#,
        feature_id
    );
    let metadata_bytes = metadata.as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "devcontainer-feature.json", metadata_bytes)
        .unwrap();

    archive.into_inner().unwrap().finish().unwrap()
}

/// Start a minimal HTTP server that serves the given bytes on any request.
/// Returns the server URL and a join handle.
async fn start_tarball_server(tarball_bytes: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://127.0.0.1:{}/feature.tar.gz", addr.port());

    let handle = tokio::spawn(async move {
        // Accept up to 2 connections (in case of retries or multi-request tests)
        for _ in 0..2 {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/gzip\r\n\r\n",
                    tarball_bytes.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.write_all(&tarball_bytes).await;
                let _ = socket.flush().await;
            }
        }
    });

    (url, handle)
}

// ==========================================================================
// Integration test: Tarball URL download (local HTTP server, no runtime)
// ==========================================================================

#[tokio::test]
async fn test_integration_tarball_url_download() {
    let tarball = create_test_feature_tarball(
        "url-test-feature",
        "#!/bin/bash\necho 'tarball feature installed'\n",
    );
    let (url, server) = start_tarball_server(tarball).await;

    let cache_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();

    let source = parse_feature_ref(&url);
    match &source {
        FeatureSource::TarballUrl { url: u } => {
            assert!(u.starts_with("http://127.0.0.1:"));
        }
        _ => panic!("Expected TarballUrl source"),
    }

    let result = features::download::download_feature(
        &source,
        config_dir.path(),
        cache_dir.path(),
        &None,
    )
    .await;

    let feature_dir = result.expect("tarball download should succeed");

    // Verify extracted files
    assert!(
        feature_dir.join("install.sh").exists(),
        "install.sh should exist"
    );
    assert!(
        feature_dir.join("devcontainer-feature.json").exists(),
        "devcontainer-feature.json should exist"
    );

    // Read and verify metadata
    let metadata = features::download::read_feature_metadata(&feature_dir);
    assert_eq!(metadata.id.as_deref(), Some("url-test-feature"));

    // Verify caching — server won't accept another request if it already served 2
    let result2 = features::download::download_feature(
        &source,
        config_dir.path(),
        cache_dir.path(),
        &None,
    )
    .await;
    let feature_dir2 = result2.expect("cached download should succeed");
    assert_eq!(feature_dir, feature_dir2, "Should return same cached path");

    server.abort();
    eprintln!("Integration tarball URL download test passed!");
}

// ==========================================================================
// E2E: Build container with tarball URL feature, verify it installed
// ==========================================================================

#[tokio::test]
#[ignore] // Requires container runtime
async fn test_e2e_tarball_url_feature_install() {
    let provider = match get_test_provider().await {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no container runtime available");
            return;
        }
    };

    // Create a tarball feature that writes a marker file during install
    let tarball = create_test_feature_tarball(
        "tarball-url-feature",
        "#!/bin/bash\necho 'tarball-feature-installed' > /tmp/tarball-feature-marker\n",
    );
    let (feature_url, server) = start_tarball_server(tarball).await;

    // Create workspace with devcontainer.json referencing the tarball URL feature
    let workspace = TempDir::new().expect("temp dir");
    let dc_dir = workspace.path().join(".devcontainer");
    std::fs::create_dir_all(&dc_dir).unwrap();
    std::fs::write(
        dc_dir.join("devcontainer.json"),
        format!(
            r#"{{
                "image": "ubuntu:22.04",
                "features": {{
                    "{}": true
                }}
            }}"#,
            feature_url
        ),
    )
    .unwrap();

    let container =
        Container::from_workspace(workspace.path()).expect("should load container config");

    // Resolve features
    let config_dir = container.config_path.parent().unwrap().to_path_buf();
    let resolved = features::resolve_and_prepare_features(
        container.devcontainer.features.as_ref().unwrap(),
        &config_dir,
        &None,
    )
    .await
    .expect("feature resolution should succeed");

    assert_eq!(resolved.len(), 1);
    assert!(
        resolved[0].dir.join("install.sh").exists(),
        "Resolved feature should have install.sh"
    );

    // Build the enhanced image with the tarball feature
    let remote_user = container.devcontainer.effective_user().unwrap_or("root");
    let enhanced_ctx = EnhancedBuildContext::from_image_with_features(
        "ubuntu:22.04",
        &resolved,
        false,
        remote_user,
    )
    .expect("enhanced build context should succeed");

    let image_tag = "devc/test-tarball-url-e2e:latest".to_string();
    let build_config = BuildConfig {
        context: enhanced_ctx.context_path().to_path_buf(),
        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
        tag: image_tag.clone(),
        build_args: HashMap::new(),
        target: None,
        cache_from: Vec::new(),
        labels: HashMap::new(),
        no_cache: false,
        pull: false,
    };

    eprintln!("Building image with tarball URL feature...");
    provider
        .build(&build_config)
        .await
        .expect("build should succeed");

    // Create and start a container
    let container_name = "devc-test-tarball-url-e2e";
    let _ = provider.remove_by_name(container_name).await;

    let feature_props = merge_feature_properties(&resolved);
    let mut config = container.create_config_with_features(&image_tag, Some(&feature_props));
    config.name = Some(container_name.to_string());
    config.cmd = Some(vec![
        "bash".to_string(),
        "-c".to_string(),
        "sleep infinity".to_string(),
    ]);

    let cid = provider.create(&config).await.expect("create should succeed");
    provider.start(&cid).await.expect("start should succeed");

    // Verify the marker file was created by the feature's install.sh
    let marker_result = provider
        .exec(
            &cid,
            &ExecConfig {
                cmd: vec!["cat".into(), "/tmp/tarball-feature-marker".into()],
                ..Default::default()
            },
        )
        .await
        .expect("should read marker file");
    assert!(
        marker_result.output.contains("tarball-feature-installed"),
        "Feature install.sh should have run, got: {}",
        marker_result.output.trim()
    );

    // Cleanup
    let _ = provider.remove(&cid, true).await;
    let runtime = if std::process::Command::new("podman")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "podman"
    } else {
        "docker"
    };
    let _ = std::process::Command::new(runtime)
        .args(["rmi", &image_tag])
        .output();
    server.abort();
    eprintln!("E2E tarball URL feature install test passed!");
}
