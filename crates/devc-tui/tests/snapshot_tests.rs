//! Snapshot tests for UI rendering using insta

use devc_core::DevcContainerStatus;
use devc_tui::{App, ConfirmAction, ContainerOperation, DialogFocus, Tab, View};
use ratatui::{backend::TestBackend, Terminal};

/// Helper to render the app and capture output as a string
fn render_app(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| devc_tui::ui::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    buffer_to_string(&buffer)
}

/// Convert a ratatui buffer to a string representation
fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = buffer.get(x, y);
            output.push_str(cell.symbol());
        }
        output.push('\n');
    }
    output
}

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
    app.containers_table_state
        .select(Some(0));

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
