//! Snapshot tests for UI rendering using insta

mod helpers;

use devc_core::DevcContainerStatus;
use devc_provider::{
    ContainerDetails, ContainerId, ContainerStatus, DevcontainerSource, DiscoveredContainer,
    MountInfo, NetworkInfo, NetworkSettings, PortInfo, ProviderType,
};
use devc_tui::{App, ConfirmAction, ContainerOperation, DialogFocus, Tab, View};

use helpers::render_app;

/// Test empty containers view rendering
#[test]
fn test_containers_view_empty() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;
    app.view = View::Main;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test containers view with items
#[test]
fn test_containers_view_with_items() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;
    app.view = View::Main;

    // Add some test containers
    app.containers = vec![
        App::create_test_container("my-rust-project", DevcContainerStatus::Running),
        App::create_test_container("python-api", DevcContainerStatus::Stopped),
        App::create_test_container("frontend-app", DevcContainerStatus::Building),
    ];
    app.selected = 0;
    app.containers_table_state.select(Some(0));

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test providers view rendering
#[test]
fn test_providers_view() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Providers;
    app.view = View::Main;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test settings view rendering
#[test]
fn test_settings_view() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Settings;
    app.view = View::Main;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test simple confirm dialog rendering
#[test]
fn test_simple_confirm_dialog() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;

    // Add a container and set up delete confirmation
    app.containers = vec![App::create_test_container(
        "test-container",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;
    app.containers_table_state.select(Some(0));
    app.confirm_action = Some(ConfirmAction::Delete("test-container".to_string()));
    app.view = View::Confirm;
    app.dialog_focus = DialogFocus::Confirm;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test rebuild confirm dialog with checkbox
#[test]
fn test_rebuild_confirm_dialog() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;

    // Add a container and set up rebuild confirmation
    app.containers = vec![App::create_test_container(
        "test-container",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;
    app.containers_table_state.select(Some(0));
    app.confirm_action = Some(ConfirmAction::Rebuild {
        id: "test-container".to_string(),
        provider_change: None,
    });
    app.view = View::Confirm;
    app.dialog_focus = DialogFocus::Checkbox;
    app.rebuild_no_cache = true;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test container detail view
#[test]
fn test_container_detail_view() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;

    // Add a container
    app.containers = vec![App::create_test_container(
        "my-rust-project",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;
    app.view = View::ContainerDetail;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test help view
#[test]
fn test_help_view() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;
    app.view = View::Help;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test build output view
#[test]
fn test_build_output_view() {
    let mut app = App::new_for_testing();
    app.view = View::BuildOutput;
    app.build_output = vec![
        "Building container: test-container".to_string(),
        "Step 1/5: FROM rust:latest".to_string(),
        "Step 2/5: WORKDIR /app".to_string(),
        "Step 3/5: COPY . .".to_string(),
    ];
    app.build_complete = false;
    app.build_auto_scroll = true;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test logs view
#[test]
fn test_logs_view() {
    let mut app = App::new_for_testing();

    // Add a container
    app.containers = vec![App::create_test_container(
        "my-rust-project",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;
    app.view = View::Logs;
    app.logs = vec![
        "2024-01-01 00:00:00 Container starting...".to_string(),
        "2024-01-01 00:00:01 Loading configuration...".to_string(),
        "2024-01-01 00:00:02 Server started on port 8080".to_string(),
    ];
    app.logs_scroll = 0;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test provider detail popup overlay
#[test]
fn test_provider_detail_popup() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Providers;
    app.selected_provider = 0;
    app.view = View::ProviderDetail;

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test ports popup overlay (empty, no ports detected)
#[test]
fn test_ports_popup_empty() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;

    // Add a running container
    app.containers = vec![App::create_test_container(
        "my-rust-project",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;
    app.containers_table_state.select(Some(0));

    // Set up ports view with no detected ports
    app.view = View::Ports;
    app.port_state.container_id = Some("test-my-rust-project".to_string());
    app.port_state.socat_installed = Some(true);

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test ports popup overlay with detected ports
#[test]
fn test_ports_popup_with_ports() {
    use devc_tui::ports::DetectedPort;

    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;

    // Add a running container
    app.containers = vec![App::create_test_container(
        "my-rust-project",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;
    app.containers_table_state.select(Some(0));

    // Set up ports view with detected ports
    app.view = View::Ports;
    app.port_state.container_id = Some("test-my-rust-project".to_string());
    app.port_state.socat_installed = Some(true);
    app.port_state.detected_ports = vec![
        DetectedPort {
            port: 3000,
            protocol: "tcp".to_string(),
            process: Some("node".to_string()),
            is_new: false,
            is_forwarded: true,
        },
        DetectedPort {
            port: 8080,
            protocol: "tcp".to_string(),
            process: Some("java".to_string()),
            is_new: true,
            is_forwarded: false,
        },
    ];
    app.port_state.selected_port = 0;
    app.port_state.table_state.select(Some(0));

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test container operation spinner modal
#[test]
fn test_container_operation_spinner() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;
    app.view = View::Main;

    // Add some containers
    app.containers = vec![
        App::create_test_container("my-rust-project", DevcContainerStatus::Running),
        App::create_test_container("python-api", DevcContainerStatus::Stopped),
    ];
    app.selected = 0;
    app.containers_table_state.select(Some(0));

    // Set up a stopping operation
    app.container_op = Some(ContainerOperation::Stopping {
        id: "test-my-rust-project".to_string(),
        name: "my-rust-project".to_string(),
    });
    app.spinner_frame = 2; // Fixed frame for deterministic snapshot

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test compose container list view with badge
#[test]
fn test_compose_container_list_badge() {
    use devc_provider::{ComposeServiceInfo, ContainerId, ContainerStatus};

    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;
    app.view = View::Main;

    // Add a compose container and a regular container
    app.containers = vec![
        App::create_test_compose_container(
            "compose-app",
            DevcContainerStatus::Running,
            "devc-compose-app",
            "app",
        ),
        App::create_test_container("standalone", DevcContainerStatus::Running),
    ];
    app.selected = 0;
    app.containers_table_state.select(Some(0));

    // Populate compose services cache for the compose container
    app.compose_state.services.insert(
        "test-compose-app".to_string(),
        vec![
            ComposeServiceInfo {
                service_name: "app".to_string(),
                container_id: ContainerId::new("container-app-123"),
                status: ContainerStatus::Running,
            },
            ComposeServiceInfo {
                service_name: "db".to_string(),
                container_id: ContainerId::new("container-db-456"),
                status: ContainerStatus::Running,
            },
            ComposeServiceInfo {
                service_name: "redis".to_string(),
                container_id: ContainerId::new("container-redis-789"),
                status: ContainerStatus::Running,
            },
        ],
    );

    let output = render_app(&mut app, 80, 24);
    insta::assert_snapshot!(output);
}

/// Test compose container detail view with services table
#[test]
fn test_compose_container_detail_with_services() {
    use devc_provider::{ComposeServiceInfo, ContainerId, ContainerStatus};
    use ratatui::widgets::TableState;

    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;

    // Add a compose container
    app.containers = vec![App::create_test_compose_container(
        "compose-app",
        DevcContainerStatus::Running,
        "devc-compose-app",
        "app",
    )];
    app.selected = 0;
    app.view = View::ContainerDetail;

    // Populate compose services
    app.compose_state.services.insert(
        "test-compose-app".to_string(),
        vec![
            ComposeServiceInfo {
                service_name: "app".to_string(),
                container_id: ContainerId::new("container-app-123"),
                status: ContainerStatus::Running,
            },
            ComposeServiceInfo {
                service_name: "db".to_string(),
                container_id: ContainerId::new("container-db-456"),
                status: ContainerStatus::Running,
            },
            ComposeServiceInfo {
                service_name: "redis".to_string(),
                container_id: ContainerId::new("container-redis-789"),
                status: ContainerStatus::Exited,
            },
        ],
    );
    app.compose_state.selected_service = 0;
    app.compose_state.services_table_state = TableState::default().with_selected(0);

    let output = render_app(&mut app, 90, 30);
    insta::assert_snapshot!(output);
}

/// Test compose container detail view loading state
#[test]
fn test_compose_container_detail_loading() {
    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;

    // Add a compose container
    app.containers = vec![App::create_test_compose_container(
        "compose-app",
        DevcContainerStatus::Running,
        "devc-compose-app",
        "app",
    )];
    app.selected = 0;
    app.view = View::ContainerDetail;
    app.compose_state.services_loading = true;

    let output = render_app(&mut app, 90, 30);
    insta::assert_snapshot!(output);
}

/// Test discover detail popup view
#[test]
fn test_discover_detail_view() {
    use std::collections::HashMap;

    let mut app = App::new_for_testing();
    app.tab = Tab::Containers;
    app.discover_mode = true;

    // Set up a discovered container
    app.discovered_containers = vec![DiscoveredContainer {
        id: ContainerId("abc123def456".to_string()),
        name: "my-devcontainer".to_string(),
        image: "mcr.microsoft.com/devcontainers/rust:1".to_string(),
        status: ContainerStatus::Running,
        source: DevcontainerSource::VsCode,
        workspace_path: Some("/home/user/project".to_string()),
        labels: HashMap::new(),
        provider: ProviderType::Docker,
        created: Some("2024-01-15 12:00:00".to_string()),
    }];
    app.selected_discovered = 0;

    // Set up inspect details
    let mut labels = HashMap::new();
    labels.insert(
        "devcontainer.local_folder".to_string(),
        "/home/user/project".to_string(),
    );
    labels.insert("devc.managed".to_string(), "true".to_string());
    labels.insert(
        "com.docker.compose.service".to_string(),
        "devcontainer".to_string(),
    );
    labels.insert("maintainer".to_string(), "dev-team".to_string());
    labels.insert("devcontainer.metadata".to_string(), "{}".to_string());

    let mut networks = HashMap::new();
    networks.insert(
        "bridge".to_string(),
        NetworkInfo {
            network_id: "net123".to_string(),
            ip_address: Some("172.17.0.2".to_string()),
            gateway: Some("172.17.0.1".to_string()),
        },
    );

    app.discover_detail = Some(ContainerDetails {
        id: ContainerId("abc123def456".to_string()),
        name: "my-devcontainer".to_string(),
        image: "mcr.microsoft.com/devcontainers/rust:1".to_string(),
        image_id: "sha256:abcdef1234567890abcdef".to_string(),
        status: ContainerStatus::Running,
        created: 1705320000, // 2024-01-15 12:00:00 UTC
        started_at: Some(1705320060),
        finished_at: None,
        exit_code: None,
        labels,
        env: vec![
            "PATH=/usr/bin".to_string(),
            "HOME=/root".to_string(),
            "RUST_LOG=debug".to_string(),
            "CARGO_HOME=/usr/local/cargo".to_string(),
            "HOSTNAME=abc123".to_string(),
        ],
        mounts: vec![
            MountInfo {
                mount_type: "bind".to_string(),
                source: "/home/user/project".to_string(),
                destination: "/workspaces/project".to_string(),
                read_only: false,
            },
            MountInfo {
                mount_type: "volume".to_string(),
                source: "vscode-extensions".to_string(),
                destination: "/root/.vscode-server/extensions".to_string(),
                read_only: false,
            },
        ],
        ports: vec![PortInfo {
            container_port: 8080,
            host_port: Some(8080),
            protocol: "tcp".to_string(),
            host_ip: Some("0.0.0.0".to_string()),
        }],
        network_settings: NetworkSettings {
            ip_address: Some("172.17.0.2".to_string()),
            gateway: Some("172.17.0.1".to_string()),
            networks,
        },
    });
    app.discover_detail_scroll = 0;
    app.view = View::DiscoverDetail;

    let output = render_app(&mut app, 90, 30);
    insta::assert_snapshot!(output);
}
