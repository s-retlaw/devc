use super::*;

pub(super) fn draw_confirm_dialog(frame: &mut Frame, app: &App, area: Rect) {
    match &app.confirm_action {
        Some(ConfirmAction::Delete(id)) => {
            let container = app.containers.iter().find(|c| &c.id == id);
            let name = container.map(|c| c.name.as_str()).unwrap_or(id);
            let is_adopted = container
                .map(|c| c.source != DevcontainerSource::Devc)
                .unwrap_or(false);
            let has_container = container.map(|c| c.container_id.is_some()).unwrap_or(false);
            let msg = if is_adopted {
                format!("Stop tracking '{}'? (container will not be deleted)", name)
            } else if has_container {
                format!("Delete container '{}'?", name)
            } else {
                format!("Remove '{}' from registry?", name)
            };
            draw_simple_confirm_dialog(frame, app, area, &msg);
        }
        Some(ConfirmAction::Stop(id)) => {
            let name = app
                .containers
                .iter()
                .find(|c| &c.id == id)
                .map(|c| c.name.as_str())
                .unwrap_or(id);
            draw_simple_confirm_dialog(frame, app, area, &format!("Stop container '{}'?", name));
        }
        Some(ConfirmAction::Rebuild {
            id,
            provider_change,
        }) => {
            let name = app
                .containers
                .iter()
                .find(|c| &c.id == id)
                .map(|c| c.name.as_str())
                .unwrap_or(id);
            draw_rebuild_confirm_dialog(frame, app, area, name, provider_change.as_ref());
        }
        Some(ConfirmAction::SetDefaultProvider(provider_type)) => {
            let provider_name = match provider_type {
                devc_provider::ProviderType::Docker => "Docker",
                devc_provider::ProviderType::Podman => "Podman",
            };
            draw_set_provider_confirm_dialog(frame, app, area, provider_name);
        }
        Some(ConfirmAction::Adopt { container_name, .. }) => {
            draw_simple_confirm_dialog(
                frame,
                app,
                area,
                &format!("Adopt '{}' into devc management?", container_name),
            );
        }
        Some(ConfirmAction::Forget { name, .. }) => {
            draw_simple_confirm_dialog(
                frame,
                app,
                area,
                &format!("Forget '{}'? (container will not be deleted)", name),
            );
        }
        Some(ConfirmAction::CancelBuild) => {
            draw_simple_confirm_dialog(frame, app, area, "Cancel build in progress?");
        }
        Some(ConfirmAction::QuitApp) => {
            draw_simple_confirm_dialog(frame, app, area, "Quit devc?");
        }
        None => {}
    }
}

/// Draw a simple yes/no confirmation dialog
pub(super) fn draw_simple_confirm_dialog(frame: &mut Frame, app: &App, area: Rect, message: &str) {
    // +4 for border (2) + padding (2); minimum 50
    let width = (message.len() as u16 + 4).max(50);
    DialogBuilder::new("Confirm")
        .width(width)
        .empty_line()
        .message(message)
        .empty_line()
        .empty_line()
        .buttons(app.dialog_focus)
        .empty_line()
        .help("Tab: Switch  Enter: Select  Esc: Cancel")
        .render(frame, area);
}

/// Draw the set default provider confirmation dialog
pub(super) fn draw_set_provider_confirm_dialog(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    provider_name: &str,
) {
    let message = format!("Set {} as default provider?", provider_name);

    DialogBuilder::new("Set Default Provider")
        .width(55)
        .border_color(Color::Cyan)
        .empty_line()
        .message(&message)
        .empty_line()
        .styled_message(Line::from(Span::styled(
            "This will save the setting and reconnect.",
            Style::default().fg(Color::DarkGray),
        )))
        .empty_line()
        .buttons(app.dialog_focus)
        .empty_line()
        .help("Tab: Switch  Enter: Select  Esc: Cancel")
        .render(frame, area);
}

/// Draw the rebuild confirmation dialog with provider change warning and no-cache toggle
pub(super) fn draw_rebuild_confirm_dialog(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    name: &str,
    provider_change: Option<&(devc_provider::ProviderType, devc_provider::ProviderType)>,
) {
    // Pre-format strings to avoid lifetime issues
    let message = format!("Rebuild '{}'?", name);
    let warning_text = provider_change.map(|(old, new)| format!("{} -> {}", old, new));

    let mut builder = DialogBuilder::new("Rebuild Container")
        .width(50)
        .empty_line()
        .message(&message)
        .empty_line();

    // Add provider change warning if applicable
    if let Some(warning) = &warning_text {
        builder = builder.styled_message(Line::from(vec![
            Span::styled("  Warning: ", Style::default().fg(Color::Yellow).bold()),
            Span::styled(warning.clone(), Style::default().fg(Color::Yellow)),
        ]));
        builder = builder.empty_line();
    }

    builder
        .checkbox(
            "Force rebuild (no cache)",
            app.rebuild_no_cache,
            app.dialog_focus == DialogFocus::Checkbox,
        )
        .empty_line()
        .buttons(app.dialog_focus)
        .empty_line()
        .help("Tab: Switch  Enter/Space: Select  Esc: Cancel")
        .render(frame, area);
}
