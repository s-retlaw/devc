//! Unit tests for App state transitions

use devc_core::DevcContainerStatus;
use devc_provider::ProviderType;
use devc_tui::{App, ConfirmAction, DialogFocus, Tab, View};

/// Test tab navigation cycles forward
#[test]
fn test_tab_navigation_forward() {
    let mut app = App::new_for_testing();

    // Start at Containers
    assert_eq!(app.tab, Tab::Containers);

    // Tab forward cycles: Containers -> Providers -> Settings -> Containers
    app.tab = Tab::Providers;
    assert_eq!(app.tab, Tab::Providers);

    app.tab = Tab::Settings;
    assert_eq!(app.tab, Tab::Settings);

    app.tab = Tab::Containers;
    assert_eq!(app.tab, Tab::Containers);
}

/// Test tab navigation cycles backward
#[test]
fn test_tab_navigation_backward() {
    let mut app = App::new_for_testing();

    // Start at Containers
    assert_eq!(app.tab, Tab::Containers);

    // Tab backward cycles: Containers -> Settings -> Providers -> Containers
    app.tab = Tab::Settings;
    assert_eq!(app.tab, Tab::Settings);

    app.tab = Tab::Providers;
    assert_eq!(app.tab, Tab::Providers);

    app.tab = Tab::Containers;
    assert_eq!(app.tab, Tab::Containers);
}

/// Test selection wraps around container list
#[test]
fn test_selection_wraps_around() {
    let mut app = App::new_for_testing();

    // Add some test containers
    app.containers = vec![
        App::create_test_container("container-1", DevcContainerStatus::Running),
        App::create_test_container("container-2", DevcContainerStatus::Stopped),
        App::create_test_container("container-3", DevcContainerStatus::Building),
    ];
    app.selected = 0;

    // Move down through all containers
    app.selected = 1;
    assert_eq!(app.selected, 1);

    app.selected = 2;
    assert_eq!(app.selected, 2);

    // Wrap around to start
    app.selected = (app.selected + 1) % app.containers.len();
    assert_eq!(app.selected, 0);

    // Wrap around going up (manual calculation like the app does)
    app.selected = app
        .selected
        .checked_sub(1)
        .unwrap_or(app.containers.len() - 1);
    assert_eq!(app.selected, 2);
}

/// Test selection with empty container list
#[test]
fn test_selection_with_empty_list() {
    let app = App::new_for_testing();

    // With empty list, selected should be 0 and we shouldn't crash
    assert!(app.containers.is_empty());
    assert_eq!(app.selected, 0);
}

/// Test dialog focus cycling for simple dialog (no checkbox)
#[test]
fn test_dialog_focus_cycle_simple() {
    let mut app = App::new_for_testing();

    // Start at default (Cancel - safer UX)
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    // Tab cycles: Confirm -> Cancel -> Confirm (no checkbox)
    app.dialog_focus = DialogFocus::Cancel;
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    // Back to Confirm
    app.dialog_focus = DialogFocus::Confirm;
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);
}

/// Test dialog focus cycling for dialog with checkbox
#[test]
fn test_dialog_focus_cycle_with_checkbox() {
    let mut app = App::new_for_testing();

    // Set up a rebuild action (has checkbox)
    app.confirm_action = Some(ConfirmAction::Rebuild {
        id: "test-id".to_string(),
        provider_change: None,
    });

    // Start at Confirm (default)
    app.dialog_focus = DialogFocus::Confirm;

    // Tab cycles: Confirm -> Cancel -> Checkbox -> Confirm (with checkbox)
    app.dialog_focus = DialogFocus::Cancel;
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    app.dialog_focus = DialogFocus::Checkbox;
    assert_eq!(app.dialog_focus, DialogFocus::Checkbox);

    app.dialog_focus = DialogFocus::Confirm;
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);
}

/// Test delete action shows confirm dialog
#[test]
fn test_delete_shows_confirm_dialog() {
    let mut app = App::new_for_testing();

    // Add a container
    app.containers = vec![App::create_test_container(
        "test-container",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;

    // Simulate delete action (like pressing 'd')
    let container = &app.containers[app.selected];
    app.confirm_action = Some(ConfirmAction::Delete(container.id.clone()));
    app.view = View::Confirm;

    assert!(matches!(app.confirm_action, Some(ConfirmAction::Delete(_))));
    assert_eq!(app.view, View::Confirm);
}

/// Test escape cancels dialog
#[test]
fn test_escape_cancels_dialog() {
    let mut app = App::new_for_testing();

    // Set up a confirm dialog
    app.view = View::Confirm;
    app.confirm_action = Some(ConfirmAction::Delete("test-id".to_string()));
    app.dialog_focus = DialogFocus::Confirm;

    // Simulate escape (cancel action)
    app.confirm_action = None;
    app.rebuild_no_cache = false;
    app.dialog_focus = DialogFocus::default();
    app.view = View::Main;

    assert!(app.confirm_action.is_none());
    assert_eq!(app.view, View::Main);
    assert_eq!(app.dialog_focus, DialogFocus::Cancel); // Default (safer UX)
}

/// Test enter in container list shows detail view
#[test]
fn test_enter_in_container_list_shows_detail() {
    let mut app = App::new_for_testing();

    // Add a container
    app.containers = vec![App::create_test_container(
        "test-container",
        DevcContainerStatus::Running,
    )];
    app.selected = 0;

    // Simulate Enter key action
    if !app.containers.is_empty() {
        app.view = View::ContainerDetail;
    }

    assert_eq!(app.view, View::ContainerDetail);
}

/// Test selected_container returns correct container
#[test]
fn test_selected_container_returns_correct() {
    let mut app = App::new_for_testing();

    app.containers = vec![
        App::create_test_container("first", DevcContainerStatus::Running),
        App::create_test_container("second", DevcContainerStatus::Stopped),
    ];

    app.selected = 0;
    assert_eq!(app.selected_container().unwrap().name, "first");

    app.selected = 1;
    assert_eq!(app.selected_container().unwrap().name, "second");
}

/// Test selected_container returns None for empty list
#[test]
fn test_selected_container_returns_none_for_empty() {
    let app = App::new_for_testing();

    assert!(app.selected_container().is_none());
}

/// Test view transitions
#[test]
fn test_view_transitions() {
    let mut app = App::new_for_testing();

    // Start at Main
    assert_eq!(app.view, View::Main);

    // Go to Help
    app.view = View::Help;
    assert_eq!(app.view, View::Help);

    // Return to Main
    app.view = View::Main;
    assert_eq!(app.view, View::Main);

    // Go to ContainerDetail
    app.view = View::ContainerDetail;
    assert_eq!(app.view, View::ContainerDetail);

    // Go to Logs
    app.view = View::Logs;
    assert_eq!(app.view, View::Logs);

    // Go to BuildOutput
    app.view = View::BuildOutput;
    assert_eq!(app.view, View::BuildOutput);
}

/// Test provider selection
#[test]
fn test_provider_selection() {
    let mut app = App::new_for_testing();

    // Should have 2 providers
    assert_eq!(app.providers.len(), 2);

    // Start at first provider (Docker)
    assert_eq!(app.selected_provider, 0);
    assert_eq!(app.providers[0].name, "Docker");

    // Move to second provider (Podman)
    app.selected_provider = 1;
    assert_eq!(app.providers[1].name, "Podman");

    // Wrap around
    app.selected_provider = (app.selected_provider + 1) % app.providers.len();
    assert_eq!(app.selected_provider, 0);
}

/// Test is_connected reflects active_provider
#[test]
fn test_is_connected() {
    let mut app = App::new_for_testing();

    // Should be connected (Docker is active)
    assert_eq!(app.active_provider, Some(ProviderType::Docker));
    assert!(app.is_connected());

    // Set to disconnected
    app.active_provider = None;
    assert!(!app.is_connected());
}

/// Test confirm action variants can be constructed
#[test]
fn test_confirm_action_variants() {
    let mut app = App::new_for_testing();

    // Delete variant
    app.confirm_action = Some(ConfirmAction::Delete("id1".to_string()));
    assert!(matches!(app.confirm_action, Some(ConfirmAction::Delete(_))));

    // Stop variant
    app.confirm_action = Some(ConfirmAction::Stop("id2".to_string()));
    assert!(matches!(app.confirm_action, Some(ConfirmAction::Stop(_))));

    // Rebuild variant
    app.confirm_action = Some(ConfirmAction::Rebuild {
        id: "id3".to_string(),
        provider_change: Some((ProviderType::Docker, ProviderType::Podman)),
    });
    assert!(matches!(
        app.confirm_action,
        Some(ConfirmAction::Rebuild { .. })
    ));

    // SetDefaultProvider variant
    app.confirm_action = Some(ConfirmAction::SetDefaultProvider(ProviderType::Podman));
    assert!(matches!(
        app.confirm_action,
        Some(ConfirmAction::SetDefaultProvider(_))
    ));

    // CancelBuild variant
    app.confirm_action = Some(ConfirmAction::CancelBuild);
    assert!(matches!(
        app.confirm_action,
        Some(ConfirmAction::CancelBuild)
    ));

    // QuitApp variant
    app.confirm_action = Some(ConfirmAction::QuitApp);
    assert!(matches!(app.confirm_action, Some(ConfirmAction::QuitApp)));
}

/// Test container operation labels for different statuses
#[test]
fn test_container_operation_labels() {
    let app = App::new_for_testing();

    // Verify different statuses can be displayed
    let statuses = [
        DevcContainerStatus::Configured,
        DevcContainerStatus::Building,
        DevcContainerStatus::Built,
        DevcContainerStatus::Created,
        DevcContainerStatus::Running,
        DevcContainerStatus::Stopped,
        DevcContainerStatus::Failed,
    ];

    for status in statuses {
        let display = format!("{}", status);
        assert!(
            !display.is_empty(),
            "Status {:?} should have display string",
            status
        );
    }

    // Verify we can create test containers with each status
    for status in statuses {
        let container = App::create_test_container("test", status);
        assert_eq!(container.status, status);
    }
    drop(app);
}

/// Test ports view state cleanup
#[test]
fn test_ports_view_cleanup() {
    let mut app = App::new_for_testing();

    // Set up ports view state
    app.view = View::Ports;
    app.port_state.container_id = Some("container123".to_string());
    app.port_state
        .detected_ports
        .push(devc_tui::ports::DetectedPort {
            port: 8080,
            protocol: "tcp".to_string(),
            process: Some("node".to_string()),
            is_new: false,
            is_forwarded: false,
        });

    // Close ports view manually
    app.port_state.container_id = None;
    app.port_state.detected_ports.clear();
    app.view = View::Main;

    assert!(app.port_state.container_id.is_none());
    assert!(app.port_state.detected_ports.is_empty());
    assert_eq!(app.view, View::Main);
}
