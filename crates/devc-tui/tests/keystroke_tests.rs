//! Integration tests that exercise App key handling via send_key()

mod helpers;

use crossterm::event::{KeyCode, KeyModifiers};
use devc_core::DevcContainerStatus;
use devc_provider::{ComposeServiceInfo, ContainerId, ContainerStatus};
use devc_tui::{App, ConfirmAction, DialogFocus, Tab, View};
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
    assert!(app.discover_mode, "Discover mode should be enabled after pressing D");

    // 'D' again toggles it off
    app.send_key(KeyCode::Char('D'), KeyModifiers::SHIFT)
        .await
        .unwrap();
    assert!(!app.discover_mode, "Discover mode should be disabled after pressing D again");
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
    assert_ne!(app.rebuild_no_cache, before, "Checkbox should toggle on Enter");
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
    app.compose_state.compose_services.insert(
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
    app.compose_state.compose_selected_service = 0;
    app.compose_state.compose_services_table_state = TableState::default().with_selected(0);

    // Enter detail view
    app.view = View::ContainerDetail;

    // 'j' should move the service selection down
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.compose_state.compose_selected_service, 1, "j should move to service index 1");

    // 'j' again moves to the third service
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.compose_state.compose_selected_service, 2, "j should move to service index 2");

    // 'k' moves back up
    app.send_key(KeyCode::Char('k'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.compose_state.compose_selected_service, 1, "k should move back to service index 1");

    // 'j' wraps around to the first service
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    app.send_key(KeyCode::Char('j'), KeyModifiers::NONE)
        .await
        .unwrap();
    assert_eq!(app.compose_state.compose_selected_service, 0, "j should wrap around to first service");
}
