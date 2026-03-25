//! Integration tests that exercise App key handling via send_key()

mod helpers;

use crossterm::event::{KeyCode, KeyModifiers};
use devc_core::DevcContainerStatus;
use devc_provider::{ComposeServiceInfo, ContainerId, ContainerStatus};
use devc_tui::{
    App, AsyncEvent, ConfirmAction, ContainerOpResult, ContainerOperation, DialogFocus, Tab, View,
};
use ratatui::widgets::TableState;

#[allow(unused_imports)]
use helpers::render_app;

/// Create an App pre-populated with three test containers
fn app_with_containers() -> App {
    let mut app = App::new_for_testing();
    app.containers = vec![
        App::create_test_container("rust-project", DevcContainerStatus::Running),
        App::create_test_container("python-api", DevcContainerStatus::Stopped),
        App::create_test_container("frontend-app", DevcContainerStatus::Building),
    ];
    app.selected = 0;
    app.containers_table_state.select(Some(0));
    app
}

// ---------------------------------------------------------------------------
// Navigation tests
// ---------------------------------------------------------------------------

/// Pressing 'j' twice moves the selection down by two
#[tokio::test]
async fn test_j_moves_selection_down() {
    let mut app = app_with_containers();
    assert_eq!(app.selected, 0);

    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.selected, 1);

    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.selected, 2);
}

/// Pressing 'k' at the top wraps selection to the last item
#[tokio::test]
async fn test_k_wraps_selection_up() {
    let mut app = app_with_containers();
    assert_eq!(app.selected, 0);

    app.send_key(KeyCode::Char('k'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.selected, 2, "k at position 0 should wrap to last item");
}

// ---------------------------------------------------------------------------
// View transition tests
// ---------------------------------------------------------------------------

/// Pressing Enter on the container list opens the detail view
#[tokio::test]
async fn test_enter_opens_detail() {
    let mut app = app_with_containers();
    assert_eq!(app.view, View::Main);

    app.send_key(KeyCode::Enter, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::ContainerDetail);
}

/// Pressing Esc from ContainerDetail returns to Main
#[tokio::test]
async fn test_escape_closes_detail() {
    let mut app = app_with_containers();

    // Enter detail view
    app.send_key(KeyCode::Enter, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::ContainerDetail);

    // Press Esc to go back (q also works but goes through "not Main" branch)
    app.send_key(KeyCode::Esc, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::Main);
}

// ---------------------------------------------------------------------------
// Tab navigation tests
// ---------------------------------------------------------------------------

/// Pressing Tab three times cycles through all tabs and back
#[tokio::test]
async fn test_tab_cycles_tabs() {
    let mut app = App::new_for_testing();
    assert_eq!(app.tab, Tab::Containers);

    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.tab, Tab::Providers);

    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.tab, Tab::Settings);

    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.tab, Tab::Containers);
}

/// Pressing '2' switches directly to the Providers tab
#[tokio::test]
async fn test_number_keys_switch_tabs() {
    let mut app = App::new_for_testing();
    assert_eq!(app.tab, Tab::Containers);

    app.send_key(KeyCode::Char('2'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.tab, Tab::Providers);

    app.send_key(KeyCode::Char('3'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.tab, Tab::Settings);

    app.send_key(KeyCode::Char('1'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.tab, Tab::Containers);
}

// ---------------------------------------------------------------------------
// Help view
// ---------------------------------------------------------------------------

/// Pressing '?' opens the Help view
#[tokio::test]
async fn test_question_mark_opens_help() {
    let mut app = App::new_for_testing();
    assert_eq!(app.view, View::Main);

    app.send_key(KeyCode::Char('?'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::Help);
}

// ---------------------------------------------------------------------------
// Delete confirmation
// ---------------------------------------------------------------------------

/// Pressing 'd' on a non-Available container opens the delete confirm dialog
#[tokio::test]
async fn test_d_shows_delete_confirm() {
    let mut app = app_with_containers();
    // Select the Stopped container (index 1) which is deletable
    app.selected = 1;
    app.containers_table_state.select(Some(1));

    app.send_key(KeyCode::Char('d'), KeyModifiers::NONE)
        .await
        .unwrap();

    assert_eq!(app.view, View::Confirm);
    assert!(
        matches!(app.confirm_action, Some(ConfirmAction::Delete(_))),
        "Expected Delete confirm action, got {:?}",
        app.confirm_action
    );
}

// ---------------------------------------------------------------------------
// Confirm dialog focus cycling
// ---------------------------------------------------------------------------

/// Tab in a simple confirm dialog cycles between Confirm and Cancel
#[tokio::test]
async fn test_confirm_tab_cycles_focus() {
    let mut app = app_with_containers();
    // Open a delete confirm dialog
    app.selected = 1;
    app.containers_table_state.select(Some(1));
    app.send_key(KeyCode::Char('d'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::Confirm);

    // Default focus should be Cancel (safer UX)
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    // Tab: Cancel -> Confirm (no checkbox for Delete dialog)
    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);

    // Tab: Confirm -> Cancel
    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);
}

/// Pressing Escape from the confirm dialog dismisses it
#[tokio::test]
async fn test_confirm_escape_cancels() {
    let mut app = app_with_containers();
    // Open a delete confirm dialog
    app.selected = 1;
    app.containers_table_state.select(Some(1));
    app.send_key(KeyCode::Char('d'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::Confirm);

    // Press Esc to dismiss
    app.send_key(KeyCode::Esc, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::Main);
    assert!(app.confirm_action.is_none());
}

// ---------------------------------------------------------------------------
// Discover mode toggle
// ---------------------------------------------------------------------------

/// Pressing Shift+D (uppercase 'D') toggles discover mode
#[tokio::test]
async fn test_d_toggles_discover_mode() {
    let mut app = app_with_containers();
    assert!(!app.discover_mode);

    // 'D' (uppercase) toggles discover mode on
    app.send_key(KeyCode::Char('D'), KeyModifiers::SHIFT)
        .await
        .unwrap();
    assert!(
        app.discover_mode,
        "Discover mode should be enabled after pressing D"
    );

    // 'D' again toggles it off
    app.send_key(KeyCode::Char('D'), KeyModifiers::SHIFT)
        .await
        .unwrap();
    assert!(
        !app.discover_mode,
        "Discover mode should be disabled after pressing D again"
    );
}

// ---------------------------------------------------------------------------
// Build output scroll
// ---------------------------------------------------------------------------

/// Pressing 'j' in BuildOutput view scrolls down
#[tokio::test]
async fn test_build_output_scroll() {
    let mut app = App::new_for_testing();
    app.view = View::BuildOutput;
    app.build_output = vec![
        "Step 1/5: FROM rust:latest".to_string(),
        "Step 2/5: WORKDIR /app".to_string(),
        "Step 3/5: COPY . .".to_string(),
        "Step 4/5: RUN cargo build".to_string(),
        "Step 5/5: CMD [\"./app\"]".to_string(),
    ];
    app.build_output_scroll = 0;
    app.build_complete = false;

    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.build_output_scroll, 1);

    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.build_output_scroll, 2);

    // Verify auto_scroll was disabled since user took control
    assert!(!app.build_auto_scroll);
}

// ---------------------------------------------------------------------------
// Rebuild dialog checkbox
// ---------------------------------------------------------------------------

/// In a Rebuild confirm dialog, Tab cycles through Checkbox -> Confirm -> Cancel
#[tokio::test]
async fn test_rebuild_dialog_checkbox() {
    let mut app = app_with_containers();
    // Manually set up a rebuild confirm dialog (start_rebuild_dialog is private)
    let container = &app.containers[app.selected];
    app.confirm_action = Some(ConfirmAction::Rebuild {
        id: container.id.clone(),
        provider_change: None,
    });
    app.dialog_focus = DialogFocus::Checkbox;
    app.view = View::Confirm;

    // Tab: Checkbox -> Confirm
    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);

    // Tab: Confirm -> Cancel
    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    // Tab: Cancel -> Checkbox (because has_checkbox is true for Rebuild)
    app.send_key(KeyCode::Tab, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Checkbox);

    // Toggle the checkbox with Enter while focused on it
    let before = app.rebuild_no_cache;
    app.send_key(KeyCode::Enter, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_ne!(
        app.rebuild_no_cache, before,
        "Checkbox should toggle on Enter"
    );
}

// ---------------------------------------------------------------------------
// Arrow key navigation in confirm dialogs
// ---------------------------------------------------------------------------

/// Left/Right arrow keys navigate between Confirm and Cancel in a simple dialog
#[tokio::test]
async fn test_confirm_arrow_keys_simple_dialog() {
    let mut app = app_with_containers();
    app.selected = 1;
    app.containers_table_state.select(Some(1));
    app.send_key(KeyCode::Char('d'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.view, View::Confirm);
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    // Left: Cancel -> Confirm
    app.send_key(KeyCode::Left, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);

    // Left at Confirm stays at Confirm (no checkbox, stop at edge)
    app.send_key(KeyCode::Left, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);

    // Right: Confirm -> Cancel
    app.send_key(KeyCode::Right, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    // Right at Cancel stays at Cancel (stop at edge)
    app.send_key(KeyCode::Right, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);
}

/// Left/Right arrow keys navigate through Checkbox -> Confirm -> Cancel in rebuild dialog
#[tokio::test]
async fn test_confirm_arrow_keys_rebuild_dialog() {
    let mut app = app_with_containers();
    let container = &app.containers[app.selected];
    app.confirm_action = Some(ConfirmAction::Rebuild {
        id: container.id.clone(),
        provider_change: None,
    });
    app.dialog_focus = DialogFocus::Checkbox;
    app.view = View::Confirm;

    // Right: Checkbox -> Confirm
    app.send_key(KeyCode::Right, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);

    // Right: Confirm -> Cancel
    app.send_key(KeyCode::Right, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Cancel);

    // Left: Cancel -> Confirm
    app.send_key(KeyCode::Left, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Confirm);

    // Left: Confirm -> Checkbox (has_checkbox is true for Rebuild)
    app.send_key(KeyCode::Left, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Checkbox);

    // Left at Checkbox stays at Checkbox (stop at edge)
    app.send_key(KeyCode::Left, KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.dialog_focus, DialogFocus::Checkbox);
}

// ---------------------------------------------------------------------------
// Compose detail service navigation
// ---------------------------------------------------------------------------

/// In compose container detail, j/k navigates the services table
#[tokio::test]
async fn test_compose_detail_service_navigation() {
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
    app.containers_table_state.select(Some(0));

    // Populate compose services for this container
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

    // Enter detail view
    app.view = View::ContainerDetail;

    // 'j' should move the service selection down
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(
        app.compose_state.selected_service, 1,
        "j should move to service index 1"
    );

    // 'j' again moves to the third service
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(
        app.compose_state.selected_service, 2,
        "j should move to service index 2"
    );

    // 'k' moves back up
    app.send_key(KeyCode::Char('k'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(
        app.compose_state.selected_service, 1,
        "k should move back to service index 1"
    );

    // 'j' wraps around to the first service
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(
        app.compose_state.selected_service, 0,
        "j should wrap around to first service"
    );
}

#[tokio::test]
async fn test_agent_sync_requires_running_container() {
    let mut app = App::new_for_testing();
    app.containers = vec![App::create_test_container(
        "python-api",
        DevcContainerStatus::Stopped,
    )];
    app.selected = 0;
    app.containers_table_state.select(Some(0));

    app.send_key(KeyCode::Char('a'), KeyModifiers::NONE)
        .await
        .unwrap();

    assert_eq!(app.view, View::Main);
    assert!(!app.loading);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Container must be running to manage agents")
    );
}

#[tokio::test]
async fn test_agent_diagnostics_view_scroll_keys() {
    let mut app = App::new_for_testing();
    app.view = View::AgentDiagnostics;
    app.agent_diagnostics_rows = vec![
        devc_tui::AgentPanelRow {
            presence: devc_core::agents::AgentContainerPresence {
                agent: devc_core::agents::AgentKind::Codex,
                enabled_effective: true,
                enabled_explicit: Some(true),
                host_available: true,
                host_reason: None,
                container_config_present: true,
                container_binary_present: true,
                warnings: vec![],
            },
            last_sync: None,
            last_sync_forced: false,
        },
        devc_tui::AgentPanelRow {
            presence: devc_core::agents::AgentContainerPresence {
                agent: devc_core::agents::AgentKind::Cursor,
                enabled_effective: true,
                enabled_explicit: Some(true),
                host_available: true,
                host_reason: None,
                container_config_present: false,
                container_binary_present: false,
                warnings: vec!["Node/npm not found".to_string()],
            },
            last_sync: None,
            last_sync_forced: false,
        },
    ];
    app.agent_diagnostics_selected = 0;
    app.agent_diagnostics_table_state.select(Some(0));

    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.agent_diagnostics_selected, 1);

    app.send_key(KeyCode::Char('G'), KeyModifiers::SHIFT)
        .await
        .unwrap();
    assert_eq!(
        app.agent_diagnostics_selected,
        app.agent_diagnostics_rows.len() - 1
    );

    app.send_key(KeyCode::Char('g'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.agent_diagnostics_selected, 0);
}

// ---------------------------------------------------------------------------
// Operation state setup tests (confirm dialog → spinner)
// ---------------------------------------------------------------------------

/// Confirming Stop sets up the Stopping operation with spinner state
#[tokio::test]
async fn test_confirm_stop_sets_operation_state() {
    let mut app = app_with_containers();
    // Container 0 is Running — set up Stop confirmation
    app.confirm_action = Some(ConfirmAction::Stop("test-rust-project".to_string()));
    app.view = View::Confirm;
    app.dialog_focus = DialogFocus::Confirm;

    // Press Enter to confirm
    app.send_key(KeyCode::Enter, KeyModifiers::NONE)
        .await
        .unwrap();

    assert!(
        matches!(app.container_op, Some(ContainerOperation::Stopping { .. })),
        "Expected Stopping op, got {:?}",
        app.container_op
    );
    assert!(app.loading);
    assert_eq!(app.spinner_frame, 0);
}

/// Confirming Delete sets up the Deleting operation with spinner state
#[tokio::test]
async fn test_confirm_delete_sets_operation_state() {
    let mut app = app_with_containers();
    // Container 1 is Stopped — set up Delete confirmation
    app.confirm_action = Some(ConfirmAction::Delete("test-python-api".to_string()));
    app.view = View::Confirm;
    app.dialog_focus = DialogFocus::Confirm;

    app.send_key(KeyCode::Enter, KeyModifiers::NONE)
        .await
        .unwrap();

    assert!(
        matches!(app.container_op, Some(ContainerOperation::Deleting { .. })),
        "Expected Deleting op, got {:?}",
        app.container_op
    );
    assert!(app.loading);
    assert_eq!(app.spinner_frame, 0);
}

/// Confirming Adopt sets up the Adopting operation with spinner state
#[tokio::test]
async fn test_confirm_adopt_sets_operation_state() {
    use devc_provider::{DevcontainerSource, ProviderType};

    let mut app = app_with_containers();
    app.confirm_action = Some(ConfirmAction::Adopt {
        container_id: "ext-123".to_string(),
        container_name: "my-devcontainer".to_string(),
        workspace_path: Some("/home/user/project".to_string()),
        source: DevcontainerSource::VsCode,
        provider: ProviderType::Docker,
    });
    app.view = View::Confirm;
    app.dialog_focus = DialogFocus::Confirm;

    app.send_key(KeyCode::Enter, KeyModifiers::NONE)
        .await
        .unwrap();

    assert!(
        matches!(app.container_op, Some(ContainerOperation::Adopting { .. })),
        "Expected Adopting op, got {:?}",
        app.container_op
    );
    assert!(app.loading);
    assert_eq!(app.spinner_frame, 0);
}

/// Confirming Forget sets up the Forgetting operation with spinner state
#[tokio::test]
async fn test_confirm_forget_sets_operation_state() {
    let mut app = app_with_containers();
    app.confirm_action = Some(ConfirmAction::Forget {
        id: "test-rust-project".to_string(),
        name: "rust-project".to_string(),
    });
    app.view = View::Confirm;
    app.dialog_focus = DialogFocus::Confirm;

    app.send_key(KeyCode::Enter, KeyModifiers::NONE)
        .await
        .unwrap();

    assert!(
        matches!(
            app.container_op,
            Some(ContainerOperation::Forgetting { .. })
        ),
        "Expected Forgetting op, got {:?}",
        app.container_op
    );
    assert!(app.loading);
    assert_eq!(app.spinner_frame, 0);
}

// ---------------------------------------------------------------------------
// Operation result handling tests (via handle_async_event)
// ---------------------------------------------------------------------------

/// Successful Starting operation clears state and sets status message
#[tokio::test]
async fn test_operation_result_starting_success() {
    let mut app = App::new_for_testing();
    app.container_op = Some(ContainerOperation::Starting {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Success(
        ContainerOperation::Starting {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(app.status_message.as_deref(), Some("Started my-app"));
}

/// Successful Stopping operation clears state and sets status message
#[tokio::test]
async fn test_operation_result_stopping_success() {
    let mut app = App::new_for_testing();
    app.container_op = Some(ContainerOperation::Stopping {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Success(
        ContainerOperation::Stopping {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(app.status_message.as_deref(), Some("Stopped my-app"));
}

/// Successful Deleting operation clears state and sets status message
#[tokio::test]
async fn test_operation_result_deleting_success() {
    let mut app = App::new_for_testing();
    app.container_op = Some(ContainerOperation::Deleting {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Success(
        ContainerOperation::Deleting {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(app.status_message.as_deref(), Some("Deleted my-app"));
}

/// Successful Up operation clears state and sets status message
#[tokio::test]
async fn test_operation_result_up_success() {
    let mut app = App::new_for_testing();
    app.container_op = Some(ContainerOperation::Up {
        id: "c1".to_string(),
        name: "my-app".to_string(),
        progress: String::new(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Success(
        ContainerOperation::Up {
            id: "c1".to_string(),
            name: "my-app".to_string(),
            progress: String::new(),
        },
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Up completed for my-app")
    );
}

/// Successful Adopting operation clears state, sets message, disables discover mode
#[tokio::test]
async fn test_operation_result_adopting_success() {
    let mut app = App::new_for_testing();
    app.discover_mode = true;
    app.container_op = Some(ContainerOperation::Adopting {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Success(
        ContainerOperation::Adopting {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(app.status_message.as_deref(), Some("Adopted my-app"));
    assert!(
        !app.discover_mode,
        "discover_mode should be false after adopt"
    );
}

/// Successful Forgetting operation clears state and sets status message
#[tokio::test]
async fn test_operation_result_forgetting_success() {
    let mut app = App::new_for_testing();
    app.container_op = Some(ContainerOperation::Forgetting {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Success(
        ContainerOperation::Forgetting {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Forgot 'my-app' (container still running)")
    );
}

/// Failed operation includes error in status message
#[tokio::test]
async fn test_operation_result_failure() {
    let mut app = App::new_for_testing();
    app.container_op = Some(ContainerOperation::Starting {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Failed(
        ContainerOperation::Starting {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
        "connection refused".to_string(),
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Start failed for my-app: connection refused")
    );
}

/// Failed Adopt includes error in status message
#[tokio::test]
async fn test_operation_result_adopt_failure() {
    let mut app = App::new_for_testing();
    app.discover_mode = true;
    app.container_op = Some(ContainerOperation::Adopting {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Failed(
        ContainerOperation::Adopting {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
        "not a devcontainer".to_string(),
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Adopt failed for my-app: not a devcontainer")
    );
    assert!(
        app.discover_mode,
        "discover_mode should stay true on failure"
    );
}

/// Failed Forget includes error in status message
#[tokio::test]
async fn test_operation_result_forget_failure() {
    let mut app = App::new_for_testing();
    app.container_op = Some(ContainerOperation::Forgetting {
        id: "c1".to_string(),
        name: "my-app".to_string(),
    });
    app.loading = true;

    app.handle_async_event(AsyncEvent::OperationComplete(ContainerOpResult::Failed(
        ContainerOperation::Forgetting {
            id: "c1".to_string(),
            name: "my-app".to_string(),
        },
        "state file locked".to_string(),
    )))
    .await
    .unwrap();

    assert!(app.container_op.is_none());
    assert!(!app.loading);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Forget failed for my-app: state file locked")
    );
}

// ---------------------------------------------------------------------------
// Container switch clears stale per-view state
// ---------------------------------------------------------------------------

/// Switching containers with 'j' clears stale logs, detail, diagnostics, and completed build output
#[tokio::test]
async fn test_container_switch_clears_stale_state() {
    let mut app = app_with_containers();

    // Populate stale per-view state as if the user had opened various views
    app.logs = vec!["old log line".to_string()];
    app.logs_scroll = 5;
    app.container_detail = Some(devc_provider::ContainerDetails {
        id: ContainerId::new("stale"),
        name: "stale".to_string(),
        image: "stale:latest".to_string(),
        image_id: "sha256:stale".to_string(),
        status: ContainerStatus::Running,
        created: 1705320000,
        started_at: None,
        finished_at: None,
        exit_code: None,
        labels: std::collections::HashMap::new(),
        env: vec![],
        mounts: vec![],
        ports: vec![],
        network_settings: devc_provider::NetworkSettings {
            ip_address: None,
            gateway: None,
            networks: std::collections::HashMap::new(),
        },
    });
    app.container_detail_scroll = 3;
    app.agent_diagnostics_container_id = Some("stale-id".to_string());
    app.agent_diagnostics_container_name = "stale-name".to_string();
    app.agent_diagnostics_title = "Stale Title".to_string();
    app.agent_diagnostics_rows = vec![devc_tui::AgentPanelRow {
        presence: devc_core::agents::AgentContainerPresence {
            agent: devc_core::agents::AgentKind::Codex,
            enabled_effective: true,
            enabled_explicit: Some(true),
            host_available: true,
            host_reason: None,
            container_config_present: true,
            container_binary_present: true,
            warnings: vec![],
        },
        last_sync: None,
        last_sync_forced: false,
    }];
    app.agent_diagnostics_selected = 1;
    app.build_output = vec!["Step 1: done".to_string()];
    app.build_output_scroll = 2;
    app.build_complete = true;

    // Press 'j' to move to next container
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.selected, 1);

    // Verify all stale state was cleared
    assert!(app.logs.is_empty(), "logs should be cleared");
    assert_eq!(app.logs_scroll, 0, "logs_scroll should be reset");
    assert!(
        app.container_detail.is_none(),
        "container_detail should be cleared"
    );
    assert_eq!(
        app.container_detail_scroll, 0,
        "container_detail_scroll should be reset"
    );
    assert!(
        app.agent_diagnostics_container_id.is_none(),
        "diagnostics container_id should be cleared"
    );
    assert!(
        app.agent_diagnostics_container_name.is_empty(),
        "diagnostics container_name should be cleared"
    );
    assert!(
        app.agent_diagnostics_title.is_empty(),
        "diagnostics title should be cleared"
    );
    assert!(
        app.agent_diagnostics_rows.is_empty(),
        "diagnostics rows should be cleared"
    );
    assert_eq!(
        app.agent_diagnostics_selected, 0,
        "diagnostics selected should be reset"
    );
    assert!(
        app.build_output.is_empty(),
        "completed build output should be cleared"
    );
    assert_eq!(
        app.build_output_scroll, 0,
        "build_output_scroll should be reset"
    );
    assert!(app.build_auto_scroll, "build_auto_scroll should be reset");
    assert!(!app.build_complete, "build_complete should be reset");
}

/// Switching containers preserves build output when a build is actively in progress
#[tokio::test]
async fn test_container_switch_preserves_active_build() {
    let mut app = app_with_containers();

    // Simulate an active build (build_complete = false, non-empty output)
    app.build_output = vec!["Step 1/3: pulling...".to_string()];
    app.build_output_scroll = 1;
    app.build_complete = false;

    // Press 'j' to switch containers
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.selected, 1);

    // Build output should be preserved since it's actively in progress
    assert_eq!(
        app.build_output.len(),
        1,
        "active build output should be preserved"
    );
    assert_eq!(
        app.build_output_scroll, 1,
        "active build scroll should be preserved"
    );
}
