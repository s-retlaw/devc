//! UI rendering for the TUI application

use ansi_to_tui::IntoText;
use crate::app::{App, ConfirmAction, ContainerOperation, DialogFocus, Tab, View};
use crate::settings::SettingsSection;
use crate::widgets::{centered_rect, DialogBuilder};
use devc_core::DevcContainerStatus;
use devc_provider::{ContainerStatus, DevcontainerSource};
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, Tabs, Wrap,
    },
};

/// Main draw function
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.size();

    // Check if we need a warning banner
    let show_warning = !app.is_connected();

    // Main layout: header with tabs, optional warning, content, footer with help
    let chunks = if show_warning {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header with tabs
                Constraint::Length(3), // Warning banner
                Constraint::Min(0),    // Content
                Constraint::Length(3), // Footer
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header with tabs
                Constraint::Min(0),    // Content
                Constraint::Length(3), // Footer
            ])
            .split(area)
    };

    draw_header_with_tabs(frame, app, chunks[0]);

    let content_area;
    let footer_area;

    if show_warning {
        draw_disconnection_warning(frame, app, chunks[1]);
        content_area = chunks[2];
        footer_area = chunks[3];
    } else {
        content_area = chunks[1];
        footer_area = chunks[2];
    }

    match app.view {
        View::Main => {
            draw_main_content(frame, app, content_area);
            if app.container_op.is_some() {
                draw_operation_progress(frame, app, area);
            }
        }
        View::ContainerDetail => {
            draw_main_content(frame, app, content_area);
            let is_compose = app.selected_container()
                .map(|c| c.compose_project.is_some())
                .unwrap_or(false);
            let popup = if is_compose {
                popup_rect(80, 85, 60, 25, content_area)
            } else {
                popup_rect(75, 70, 56, 17, content_area)
            };
            frame.render_widget(Clear, popup);
            draw_detail(frame, app, popup);
            if app.container_op.is_some() {
                draw_operation_progress(frame, app, area);
            }
        }
        View::ProviderDetail => {
            draw_main_content(frame, app, content_area);
            let popup = popup_rect(75, 75, 58, 18, content_area);
            frame.render_widget(Clear, popup);
            draw_provider_detail(frame, app, popup);
        }
        View::BuildOutput => draw_build_output(frame, app, content_area),
        View::Logs => draw_logs(frame, app, content_area),
        View::Ports => {
            draw_main_content(frame, app, content_area);
            let port_rows = app.detected_ports.len().max(3) as u16;
            let h = (port_rows + 7).max(12);
            let popup = popup_rect(80, 70, 56, h, content_area);
            frame.render_widget(Clear, popup);
            draw_ports(frame, app, popup);
            if app.socat_installing {
                draw_install_progress(frame, app, area);
            }
        }
        View::Help => draw_help(frame, app, content_area),
        View::Confirm => {
            draw_main_content(frame, app, content_area);
            draw_confirm_dialog(frame, app, area);
        }
        View::DiscoverDetail => {
            draw_main_content(frame, app, content_area);
            let popup = popup_rect(75, 75, 58, 20, content_area);
            frame.render_widget(Clear, popup);
            draw_discover_detail(frame, app, popup);
        }
        View::Shell => {
            // Shell mode is handled before drawing - this shouldn't be reached
            // but we need to handle it for exhaustive matching
        }
    }

    draw_footer(frame, app, footer_area);
}

/// Draw the main tab content (containers/providers/settings list)
fn draw_main_content(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.tab {
        Tab::Containers => {
            if app.discover_mode {
                draw_discovered_containers(frame, app, area);
            } else {
                draw_containers(frame, app, area);
            }
        }
        Tab::Providers => draw_providers(frame, app, area),
        Tab::Settings => draw_settings(frame, app, area),
    }
}

/// Calculate a popup rectangle centered in the given area with percentage-based sizing and minimums
fn popup_rect(pct_w: u16, pct_h: u16, min_w: u16, min_h: u16, area: Rect) -> Rect {
    let w = ((area.width as u32 * pct_w as u32) / 100) as u16;
    let h = ((area.height as u32 * pct_h as u32) / 100) as u16;
    let w = w.max(min_w).min(area.width);
    let h = h.max(min_h).min(area.height);
    centered_rect(w, h, area)
}

/// Draw disconnection warning banner
fn draw_disconnection_warning(frame: &mut Frame, app: &App, area: Rect) {
    let message = app
        .connection_error
        .as_deref()
        .unwrap_or("Not connected to container provider");

    let warning = Paragraph::new(Line::from(vec![
        Span::styled(" ⚠ ", Style::default().fg(Color::Yellow).bold()),
        Span::styled("DISCONNECTED: ", Style::default().fg(Color::Yellow).bold()),
        Span::styled(message, Style::default().fg(Color::White)),
        Span::styled(" - Go to Providers tab and press 'c' to retry connection", Style::default().fg(Color::Gray)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(Color::Rgb(60, 40, 0))),
    );

    frame.render_widget(warning, area);
}

/// Draw header with tab bar
fn draw_header_with_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::all()
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let number = format!("{}:", i + 1);
            let label = if *tab == Tab::Settings && app.settings_state.dirty() {
                format!("{}*", tab.label())
            } else {
                tab.label().to_string()
            };
            if *tab == app.tab {
                Line::from(vec![
                    Span::styled(number, Style::default().fg(Color::Yellow)),
                    Span::styled(label, Style::default().fg(Color::White).bold()),
                ])
            } else {
                Line::from(vec![
                    Span::styled(number, Style::default().fg(Color::DarkGray)),
                    Span::styled(label, Style::default().fg(Color::Gray)),
                ])
            }
        })
        .collect();

    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .title(" devc - Dev Container Manager ")
                .title_style(Style::default().fg(Color::Cyan).bold())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .select(app.tab.index())
        .style(Style::default())
        .highlight_style(Style::default())
        .divider(" │ ");

    frame.render_widget(tabs, area);
}

/// Build context-sensitive footer help for the container list view
fn container_list_footer(app: &App) -> String {
    if app.containers.is_empty() {
        return "D: Discover  ?: Help  q: Quit".to_string();
    }

    let status = app.selected_container().map(|c| c.status);
    let mut keys = Vec::new();

    if let Some(st) = status {
        match st {
            DevcContainerStatus::Running => keys.push("s: Stop"),
            DevcContainerStatus::Stopped | DevcContainerStatus::Created => keys.push("s: Start"),
            _ => {}
        }
        match st {
            DevcContainerStatus::Available
            | DevcContainerStatus::Configured
            | DevcContainerStatus::Built
            | DevcContainerStatus::Created
            | DevcContainerStatus::Stopped
            | DevcContainerStatus::Failed => keys.push("u: Up"),
            _ => {}
        }
        if st == DevcContainerStatus::Available {
            keys.push("b: Build");
        }
        if st != DevcContainerStatus::Building && st != DevcContainerStatus::Available {
            keys.push("R: Rebuild");
        }
        if st == DevcContainerStatus::Running {
            keys.push("p: Ports");
            keys.push("S: Shell");
            keys.push("l: Logs");
        }
        if st != DevcContainerStatus::Building && st != DevcContainerStatus::Available {
            keys.push("d: Delete");
        }
    }

    // Show forget option for non-devc containers
    if let Some(container) = app.selected_container() {
        if container.source != DevcontainerSource::Devc && !container.status.is_available() {
            keys.push("f: Forget");
        }
    }

    let action_part = keys.join("  ");
    if action_part.is_empty() {
        "D: Discover  j/k: Navigate  Enter: Details  ?: Help  q: Quit".to_string()
    } else {
        format!("D: Discover  j/k: Navigate  Enter: Details  {}  ?: Help  q: Quit", action_part)
    }
}

/// Build context-sensitive footer help for the container detail view
fn container_detail_footer(app: &App) -> String {
    let has_services = app.selected_container()
        .and_then(|c| app.compose_services.get(&c.id))
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    let status = app.selected_container().map(|c| c.status);
    let mut keys = Vec::new();

    if has_services {
        keys.push("j/k: Select service");
    } else {
        keys.push("j/k: Scroll");
    }

    if let Some(st) = status {
        match st {
            DevcContainerStatus::Running => keys.push("s: Stop"),
            DevcContainerStatus::Stopped | DevcContainerStatus::Created => keys.push("s: Start"),
            _ => {}
        }
        match st {
            DevcContainerStatus::Available
            | DevcContainerStatus::Configured
            | DevcContainerStatus::Built
            | DevcContainerStatus::Created
            | DevcContainerStatus::Stopped
            | DevcContainerStatus::Failed => keys.push("u: Up"),
            _ => {}
        }
        if st == DevcContainerStatus::Available {
            keys.push("b: Build");
        }
        if st != DevcContainerStatus::Building && st != DevcContainerStatus::Available {
            keys.push("R: Rebuild");
        }
        if st == DevcContainerStatus::Running {
            keys.push("l: Logs");
            keys.push("S: Shell");
        }
        if st != DevcContainerStatus::Building && st != DevcContainerStatus::Available {
            keys.push("d: Delete");
        }
    }

    let action_part = keys.join("  ");
    if action_part.is_empty() {
        "1-3: Switch tab  Esc/q: Back  ?: Help".to_string()
    } else {
        format!("{}  1-3: Switch tab  Esc/q: Back  ?: Help", action_part)
    }
}

/// Draw the footer with context-sensitive help
fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let help_text: String = match app.view {
        View::Main => match app.tab {
            Tab::Containers => {
                if app.discover_mode {
                    "Esc/q: Exit  j/k: Navigate  Enter: Details  a: Adopt  r: Refresh  ?: Help".to_string()
                } else {
                    container_list_footer(app)
                }
            }
            Tab::Providers => {
                "Tab/1-3: Switch tabs  j/k: Navigate  Enter: Configure  Space/a: Set Active  s: Save  ?: Help  q: Quit".to_string()
            }
            Tab::Settings => {
                if app.settings_state.editing {
                    "Enter: Confirm  Esc: Cancel  Type to edit".to_string()
                } else {
                    "Tab/1-3: Switch tabs  j/k: Navigate  Enter/Space: Edit  s: Save  r: Reset  ?: Help  q: Quit".to_string()
                }
            }
        },
        View::ContainerDetail => container_detail_footer(app),
        View::ProviderDetail => {
            if app.provider_detail_state.editing {
                "Enter: Confirm  Esc: Cancel  Type to edit".to_string()
            } else {
                "e: Edit Socket  t: Test  a/Space: Set Active  s: Save  1-3: Switch tab  Esc/q: Back".to_string()
            }
        }
        View::BuildOutput => {
            if app.build_complete {
                "j/k: Scroll  g/G: Top/Bottom  c: Copy  q/Esc: Close".to_string()
            } else {
                "j/k: Scroll  g/G: Top/Bottom  c: Copy  (building...)".to_string()
            }
        }
        View::Logs => "j/k: Scroll  g/G: Top/Bottom  PgUp/PgDn: Page  r: Refresh  Esc/q: Back".to_string(),
        View::Ports => {
            // Show install option if socat not installed
            if app.socat_installed == Some(false) && !app.socat_installing {
                "[i]nstall socat  j/k: Navigate  1-3: Switch tab  q/Esc: Back".to_string()
            } else if app.socat_installing {
                "Installing socat...  q/Esc: Back".to_string()
            } else {
                let is_forwarded = app
                    .detected_ports
                    .get(app.selected_port)
                    .map(|p| p.is_forwarded)
                    .unwrap_or(false);
                if is_forwarded {
                    "[s]top  [o]pen browser  [n]one  j/k: Navigate  1-3: Switch tab  q/Esc: Back".to_string()
                } else {
                    "[f]orward  [a]ll  j/k: Navigate  1-3: Switch tab  q/Esc: Back".to_string()
                }
            }
        }
        View::Help => "Press any key to close".to_string(),
        View::Confirm => {
            if matches!(app.confirm_action, Some(ConfirmAction::Rebuild { .. })) {
                "y/Enter: Confirm  n/Esc: Cancel  Space: Toggle no-cache".to_string()
            } else {
                "y/Enter: Yes  n/Esc: No".to_string()
            }
        }
        View::DiscoverDetail => {
            let can_adopt = app.discovered_containers.get(app.selected_discovered)
                .map(|c| c.source != DevcontainerSource::Devc).unwrap_or(false);
            if can_adopt {
                "j/k: Scroll  a: Adopt  Esc: Back".to_string()
            } else {
                "j/k: Scroll  Esc: Back".to_string()
            }
        }
        View::Shell => "Ctrl+\\ to detach and return to TUI (session preserved)".to_string(),
    };

    let status = app.status_message.as_deref().unwrap_or("");

    let footer_text = if status.is_empty() {
        help_text
    } else {
        format!("{} │ {}", status, help_text)
    };

    let footer = Paragraph::new(footer_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));

    frame.render_widget(footer, area);
}

/// Draw the containers tab using Table widget with headers
fn draw_containers(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.containers.is_empty() {
        let empty = Paragraph::new(
            "No containers found.\n\n\
             Use 'devc init' in a directory with devcontainer.json to add a container.\n\n\
             Press 'D' to discover existing devcontainers.",
        )
        .style(Style::default().fg(Color::DarkGray))
        .block(
            Block::default()
                .title(" Containers ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });

        frame.render_widget(empty, area);
        return;
    }

    // Define header row
    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("Name"),
        Cell::from("Source"),
        Cell::from("Status"),
        Cell::from("Provider"),
        Cell::from("Workspace"),
    ])
    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    // Build data rows
    let rows: Vec<Row> = app
        .containers
        .iter()
        .map(|container| {
            let status_symbol = match container.status {
                DevcContainerStatus::Available => "◌",
                DevcContainerStatus::Running => "●",
                DevcContainerStatus::Stopped => "○",
                DevcContainerStatus::Building => "◐",
                DevcContainerStatus::Built => "◑",
                DevcContainerStatus::Created => "◔",
                DevcContainerStatus::Failed => "✗",
                DevcContainerStatus::Configured => "◯",
            };

            let status_color = match container.status {
                DevcContainerStatus::Available => Color::DarkGray,
                DevcContainerStatus::Running => Color::Green,
                DevcContainerStatus::Stopped => Color::DarkGray,
                DevcContainerStatus::Building => Color::Yellow,
                DevcContainerStatus::Built => Color::Blue,
                DevcContainerStatus::Created => Color::Cyan,
                DevcContainerStatus::Failed => Color::Red,
                DevcContainerStatus::Configured => Color::DarkGray,
            };

            // Format workspace path - show last component or truncate if too long
            let workspace = container.workspace_path.display().to_string();
            let workspace_display = if workspace.len() > 35 {
                format!("...{}", &workspace[workspace.len()-32..])
            } else {
                workspace
            };

            // Show [S] indicator if there's an active shell session for this container
            let has_shell = app.shell_sessions.contains_key(&container.id);
            let name_display = if has_shell {
                format!("{} [S]", container.name)
            } else if container.compose_project.is_some() {
                let suffix = match app.compose_services.get(&container.id) {
                    Some(s) => format!(":{}", s.len()),
                    None => "...".to_string(),
                };
                format!("{} [compose{}]", container.name, suffix)
            } else {
                container.name.to_string()
            };

            Row::new(vec![
                Cell::from(status_symbol).style(Style::default().fg(status_color)),
                Cell::from(name_display).style(Style::default().bold()),
                Cell::from(container.source.to_string()).style(Style::default().fg(Color::DarkGray)),
                Cell::from(container.status.to_string()).style(Style::default().fg(status_color)),
                Cell::from(container.provider.to_string()),
                Cell::from(workspace_display).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Define column widths
    let widths = [
        Constraint::Length(3),   // Status icon
        Constraint::Length(24),  // Name
        Constraint::Length(8),   // Source
        Constraint::Length(12),  // Status
        Constraint::Length(8),   // Provider
        Constraint::Min(10),     // Workspace (takes remaining)
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(" Containers ")
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.containers_table_state);
}

/// Draw discovered containers using Table widget with headers
fn draw_discovered_containers(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.discovered_containers.is_empty() {
        let empty = Paragraph::new(
            "No devcontainers found.\n\n\
             Make sure a container provider is running and has devcontainers.",
        )
        .style(Style::default().fg(Color::DarkGray))
        .block(
            Block::default()
                .title(" Discovered Containers (Esc to exit) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: true });

        frame.render_widget(empty, area);
        return;
    }

    // Define header row
    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("Name"),
        Cell::from("Status"),
        Cell::from("Provider"),
        Cell::from("Source"),
        Cell::from("Workspace"),
    ])
    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    // Build data rows
    let rows: Vec<Row> = app
        .discovered_containers
        .iter()
        .map(|container| {
            let status_symbol = match container.status {
                ContainerStatus::Running => "●",
                ContainerStatus::Exited => "○",
                ContainerStatus::Created => "◔",
                _ => "?",
            };

            let status_color = match container.status {
                ContainerStatus::Running => Color::Green,
                ContainerStatus::Exited => Color::DarkGray,
                ContainerStatus::Created => Color::Cyan,
                _ => Color::Yellow,
            };

            let source_str = match container.source {
                DevcontainerSource::Devc => "devc",
                DevcontainerSource::VsCode => "vscode",
                DevcontainerSource::DevPod => "devpod",
                DevcontainerSource::Other => "other",
            };

            let workspace = container.workspace_path.as_deref().unwrap_or("-");
            let workspace_display = if workspace.len() > 30 {
                format!("...{}", &workspace[workspace.len()-27..])
            } else {
                workspace.to_string()
            };

            let name_display = if container.name.len() > 20 {
                format!("{}...", &container.name[..17])
            } else {
                container.name.clone()
            };

            let provider_str = format!("{}", container.provider);

            Row::new(vec![
                Cell::from(status_symbol).style(Style::default().fg(status_color)),
                Cell::from(name_display).style(Style::default().bold()),
                Cell::from(format!("{}", container.status)).style(Style::default().fg(status_color)),
                Cell::from(provider_str).style(Style::default().fg(Color::Blue)),
                Cell::from(source_str).style(Style::default().fg(Color::Cyan)),
                Cell::from(workspace_display).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Define column widths
    let widths = [
        Constraint::Length(3),   // Status icon
        Constraint::Length(22),  // Name
        Constraint::Length(10),  // Status
        Constraint::Length(8),   // Provider
        Constraint::Length(8),   // Source
        Constraint::Min(20),     // Workspace (takes remaining)
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(" Discovered Containers (Esc to exit, a to adopt) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.discovered_table_state);
}

/// Draw the providers tab
fn draw_providers(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.providers.is_empty() {
        let empty = Paragraph::new("No providers available.")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .title(" Providers - Container Runtimes ")
                    .borders(Borders::ALL),
            );
        frame.render_widget(empty, area);
        return;
    }

    // Define header row
    let header = Row::new(vec![
        Cell::from("Active"),
        Cell::from("Provider"),
        Cell::from("Status"),
        Cell::from("Socket"),
    ])
    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    // Build data rows
    let rows: Vec<Row> = app
        .providers
        .iter()
        .map(|provider| {
            let active_indicator = if provider.is_active { "●" } else { "○" };
            let active_style = if provider.is_active {
                Style::default().fg(Color::Green).bold()
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let status_text = if provider.connected {
                "Connected"
            } else {
                "Not connected"
            };
            let status_style = if provider.connected {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };

            Row::new(vec![
                Cell::from(Span::styled(active_indicator, active_style)),
                Cell::from(provider.name.as_str()),
                Cell::from(Span::styled(status_text, status_style)),
                Cell::from(Span::styled(
                    provider.socket.as_str(),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),  // Active
            Constraint::Length(10), // Provider
            Constraint::Length(15), // Status
            Constraint::Min(30),    // Socket
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Providers - Container Runtimes ")
            .borders(Borders::ALL),
    )
    .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
    .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.providers_table_state);
}

/// Draw the global settings tab with sections
fn draw_settings(frame: &mut Frame, app: &App, area: Rect) {
    let settings = &app.settings_state;

    let mut items: Vec<ListItem> = Vec::new();

    // Add a header explanation
    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            " Global settings that apply to all containers:",
            Style::default().fg(Color::DarkGray).italic(),
        ),
    ])));

    let mut field_index = 0;
    for section in SettingsSection::all() {
        // Section header
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!(" {}", section.label()),
                Style::default().fg(Color::Cyan).bold(),
            ),
        ])));

        // Fields in this section
        for field in section.fields() {
            let is_focused = settings.focused == field_index;
            let value = settings.draft.get_value(field);
            let field_dirty = value != settings.saved.get_value(field);
            let label = if field_dirty {
                format!("{}*", field.label())
            } else {
                field.label().to_string()
            };

            let display_value = if settings.editing && is_focused {
                // Show edit buffer with cursor
                let cursor_pos = settings.cursor();
                let before = &settings.edit_buffer()[..cursor_pos];
                let after = &settings.edit_buffer()[cursor_pos..];
                format!("{}│{}", before, after)
            } else if field.is_toggle() {
                if value == "true" {
                    "[●] Enabled  [ ] Disabled".to_string()
                } else {
                    "[ ] Enabled  [●] Disabled".to_string()
                }
            } else if value.is_empty() {
                "(not set)".to_string()
            } else {
                value.clone()
            };

            let style = if is_focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };

            let line = Line::from(vec![
                Span::styled(format!("   {:<20}", label), style.bold()),
                Span::styled(display_value, style),
            ]);

            items.push(ListItem::new(line).style(style));
            field_index += 1;
        }
    }

    let title = if settings.dirty() {
        " Settings (unsaved changes) "
    } else {
        " Settings "
    };

    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(if settings.dirty() {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            }),
    );

    frame.render_widget(list, area);
}

/// Draw the provider detail/configuration view
fn draw_provider_detail(frame: &mut Frame, app: &App, area: Rect) {
    let provider = &app.providers[app.selected_provider];
    let detail_state = &app.provider_detail_state;

    let mut lines: Vec<Line> = Vec::new();

    // Provider name as title
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Provider: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&provider.name, Style::default().bold()),
        if provider.is_active {
            Span::styled(" (ACTIVE)", Style::default().fg(Color::Green).bold())
        } else {
            Span::raw("")
        },
    ]));
    lines.push(Line::from(""));

    // Socket path (editable)
    let socket_label = "Socket Path:";
    let socket_value = if detail_state.editing {
        let cursor_pos = detail_state.cursor();
        let before = &detail_state.edit_buffer()[..cursor_pos];
        let after = &detail_state.edit_buffer()[cursor_pos..];
        format!("{}│{}", before, after)
    } else {
        provider.socket.clone()
    };

    let socket_style = if detail_state.editing {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    } else {
        Style::default()
    };

    lines.push(Line::from(vec![
        Span::styled(format!("{:<16}", socket_label), Style::default().fg(Color::DarkGray)),
        Span::styled(socket_value, socket_style),
        if !detail_state.editing {
            Span::styled("  [e] to edit", Style::default().fg(Color::DarkGray).italic())
        } else {
            Span::raw("")
        },
    ]));
    lines.push(Line::from(""));

    // Connection status
    let connection_line = match detail_state.connection_status {
        Some(true) => Line::from(vec![
            Span::styled("Connection:     ", Style::default().fg(Color::DarkGray)),
            Span::styled("● Connected", Style::default().fg(Color::Green).bold()),
        ]),
        Some(false) => {
            let error_msg = detail_state.connection_error.as_deref().unwrap_or("Unknown error");
            Line::from(vec![
                Span::styled("Connection:     ", Style::default().fg(Color::DarkGray)),
                Span::styled("✗ Failed: ", Style::default().fg(Color::Red).bold()),
                Span::styled(error_msg, Style::default().fg(Color::Red)),
            ])
        }
        None => {
            // Show initial status based on provider connected flag
            if provider.connected {
                Line::from(vec![
                    Span::styled("Connection:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled("● Connected", Style::default().fg(Color::Green)),
                    Span::styled("  [t] to test", Style::default().fg(Color::DarkGray).italic()),
                ])
            } else {
                Line::from(vec![
                    Span::styled("Connection:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled("○ Not tested", Style::default().fg(Color::Yellow)),
                    Span::styled("  [t] to test", Style::default().fg(Color::DarkGray).italic()),
                ])
            }
        }
    };
    lines.push(connection_line);
    lines.push(Line::from(""));

    // Tips section
    lines.push(Line::from(vec![
        Span::styled("─── Tips ", Style::default().fg(Color::DarkGray)),
        Span::styled("─".repeat(40), Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(""));

    match provider.provider_type {
        devc_provider::ProviderType::Docker => {
            lines.push(Line::from(vec![
                Span::styled("  • Start Docker: ", Style::default().fg(Color::DarkGray)),
                Span::styled("sudo systemctl start docker", Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  • Default socket: ", Style::default().fg(Color::DarkGray)),
                Span::styled("/var/run/docker.sock", Style::default().fg(Color::White)),
            ]));
        }
        devc_provider::ProviderType::Podman => {
            lines.push(Line::from(vec![
                Span::styled("  • Start Podman: ", Style::default().fg(Color::DarkGray)),
                Span::styled("systemctl --user start podman.socket", Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  • Default socket: ", Style::default().fg(Color::DarkGray)),
                Span::styled("$XDG_RUNTIME_DIR/podman/podman.sock", Style::default().fg(Color::White)),
            ]));
        }
    }

    let title = format!(" {} Configuration ", provider.name);

    let detail = Paragraph::new(lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(if detail_state.dirty {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Cyan)
                }),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, area);
}

/// Build the info text lines for the container detail view
fn build_detail_text(
    container: &devc_core::ContainerState,
    details: Option<&devc_provider::ContainerDetails>,
) -> Vec<Line<'static>> {
    let status_color = match container.status {
        DevcContainerStatus::Available => Color::DarkGray,
        DevcContainerStatus::Running => Color::Green,
        DevcContainerStatus::Stopped => Color::DarkGray,
        DevcContainerStatus::Building => Color::Yellow,
        DevcContainerStatus::Built => Color::Blue,
        DevcContainerStatus::Created => Color::Cyan,
        DevcContainerStatus::Failed => Color::Red,
        DevcContainerStatus::Configured => Color::DarkGray,
    };

    let mut lines = vec![
        Line::from(vec![
            Span::raw("Name:        "),
            Span::styled(container.name.clone(), Style::default().bold()),
        ]),
        Line::from(vec![
            Span::raw("Status:      "),
            Span::styled(
                container.status.to_string(),
                Style::default().fg(status_color).bold(),
            ),
        ]),
        Line::from(vec![
            Span::raw("Provider:    "),
            Span::raw(container.provider.to_string()),
        ]),
        Line::from(vec![
            Span::raw("Source:      "),
            Span::styled(format!("{:?}", container.source), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("ID:          "),
            Span::styled(container.id.clone(), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Workspace:   "),
            Span::raw(container.workspace_path.to_string_lossy().into_owned()),
        ]),
        Line::from(vec![
            Span::raw("Config:      "),
            Span::raw(container.config_path.to_string_lossy().into_owned()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Image ID:    "),
            Span::raw(container.image_id.as_deref().unwrap_or("Not built").to_string()),
        ]),
        Line::from(vec![
            Span::raw("Container:   "),
            Span::raw(container.container_id.as_deref().unwrap_or("Not created").to_string()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Created:     "),
            Span::raw(container.created_at.format("%Y-%m-%d %H:%M:%S").to_string()),
        ]),
        Line::from(vec![
            Span::raw("Last used:   "),
            Span::raw(container.last_used.format("%Y-%m-%d %H:%M:%S").to_string()),
        ]),
    ];

    // Add inspect-based sections when available
    if let Some(details) = details {
        if let Some(code) = details.exit_code {
            let color = if code == 0 { Color::Green } else { Color::Red };
            lines.push(Line::from(vec![
                Span::raw("Exit Code:   "),
                Span::styled(code.to_string(), Style::default().fg(color)),
            ]));
        }

        // Ports
        if !details.ports.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("─── Ports ───", Style::default().fg(Color::DarkGray))));
            for p in &details.ports {
                let host = p.host_port.map(|hp| hp.to_string()).unwrap_or_else(|| "-".to_string());
                lines.push(Line::from(format!(
                    "  {}:{} → {}",
                    host, p.container_port, p.protocol,
                )));
            }
        }

        // Mounts (all types)
        if !details.mounts.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("─── Mounts ───", Style::default().fg(Color::DarkGray))));
            for m in &details.mounts {
                let ro = if m.read_only { " (ro)" } else { "" };
                lines.push(Line::from(format!(
                    "  [{}] {} → {}{}",
                    m.mount_type, m.source, m.destination, ro,
                )));
            }
        }

        // Networks
        let has_network = details.network_settings.ip_address.is_some()
            || details.network_settings.gateway.is_some()
            || !details.network_settings.networks.is_empty();
        if has_network {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("─── Network ───", Style::default().fg(Color::DarkGray))));
            if let Some(ip) = &details.network_settings.ip_address {
                lines.push(Line::from(vec![
                    Span::raw("IP:          "),
                    Span::raw(ip.clone()),
                ]));
            }
            if let Some(gw) = &details.network_settings.gateway {
                lines.push(Line::from(vec![
                    Span::raw("Gateway:     "),
                    Span::raw(gw.clone()),
                ]));
            }
            let mut net_names: Vec<_> = details.network_settings.networks.keys().collect();
            net_names.sort();
            for net_name in net_names {
                let net_info = &details.network_settings.networks[net_name];
                let mut parts = vec![Span::raw(format!("  {}:", net_name))];
                if let Some(ip) = &net_info.ip_address {
                    parts.push(Span::raw(format!(" {}", ip)));
                }
                if let Some(gw) = &net_info.gateway {
                    parts.push(Span::styled(format!(" (gw {})", gw), Style::default().fg(Color::DarkGray)));
                }
                lines.push(Line::from(parts));
            }
        }

        // Labels
        if !details.labels.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("─── Labels ───", Style::default().fg(Color::DarkGray))));

            let well_known = [
                "devcontainer.local_folder",
                "devcontainer.config_file",
                "devc.managed",
                "devc.project",
                "devc.workspace",
                "com.docker.compose.service",
                "com.docker.compose.project",
            ];
            for key in well_known {
                if let Some(val) = details.labels.get(key) {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}: ", key), Style::default().fg(Color::Cyan)),
                        Span::raw(val.clone()),
                    ]));
                }
            }

            let mut remaining: Vec<_> = details.labels.iter()
                .filter(|(k, _)| !well_known.contains(&k.as_str()) && k.as_str() != "devcontainer.metadata")
                .collect();
            remaining.sort_by_key(|(k, _)| (*k).clone());
            for (key, val) in remaining {
                lines.push(Line::from(format!("  {}: {}", key, val)));
            }
        }

        // Environment
        if !details.env.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("─── Environment ───", Style::default().fg(Color::DarkGray))));

            let skip_prefixes = [
                "PATH=", "HOME=", "HOSTNAME=", "TERM=", "LANG=", "SHELL=",
                "USER=", "SHLVL=", "PWD=", "OLDPWD=", "LC_", "LESSOPEN=",
                "LESSCLOSE=", "LS_COLORS=", "_=",
            ];
            let mut env_sorted = details.env.clone();
            env_sorted.sort();
            for var in &env_sorted {
                if !skip_prefixes.iter().any(|p| var.starts_with(p)) {
                    lines.push(Line::from(format!("  {}", var)));
                }
            }
        }
    }

    lines
}

/// Draw the container detail view
fn draw_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let container = match app.selected_container() {
        Some(c) => c.clone(),
        None => return,
    };

    let is_compose = container.compose_project.is_some();
    let text = build_detail_text(&container, app.container_detail.as_ref());

    if is_compose {
        // For compose containers, render outer block then split into info + services
        let outer_block = Block::default()
            .title(format!(" {} ", container.name))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        let inner_area = outer_block.inner(area);
        frame.render_widget(outer_block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(12),  // Info paragraph
                Constraint::Min(6),   // Services table
            ])
            .split(inner_area);

        let info = Paragraph::new(text).wrap(Wrap { trim: true });
        frame.render_widget(info, chunks[0]);

        draw_compose_services(frame, app, &container, chunks[1]);
    } else {
        // Non-compose: scrollable Paragraph
        let detail = Paragraph::new(text)
            .block(
                Block::default()
                    .title(format!(" {} ", container.name))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: true })
            .scroll((app.container_detail_scroll as u16, 0));

        frame.render_widget(detail, area);
    }
}

/// Build detail text lines from a ContainerDetails (discovered container inspect)
fn build_discover_detail_text(
    details: &devc_provider::ContainerDetails,
    discovered: &devc_provider::DiscoveredContainer,
) -> Vec<Line<'static>> {
    use devc_provider::ContainerStatus;

    let status_color = match details.status {
        ContainerStatus::Running => Color::Green,
        ContainerStatus::Exited | ContainerStatus::Dead => Color::Red,
        ContainerStatus::Paused => Color::Yellow,
        ContainerStatus::Created => Color::Cyan,
        _ => Color::DarkGray,
    };

    let format_ts = |ts: i64| -> String {
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "N/A".to_string())
    };

    let mut lines = vec![
        Line::from(Span::styled("─── Identity ───", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::raw("Name:        "),
            Span::styled(details.name.clone(), Style::default().bold()),
        ]),
        Line::from(vec![
            Span::raw("ID:          "),
            Span::styled(
                details.id.0.chars().take(12).collect::<String>(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::raw("Image:       "),
            Span::raw(details.image.clone()),
        ]),
        Line::from(vec![
            Span::raw("Image ID:    "),
            Span::styled(
                details.image_id.chars().take(19).collect::<String>(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::raw("Provider:    "),
            Span::raw(discovered.provider.to_string()),
        ]),
        Line::from(vec![
            Span::raw("Source:      "),
            Span::styled(format!("{:?}", discovered.source), Style::default().fg(Color::Cyan)),
        ]),
    ];
    if let Some(ws) = &discovered.workspace_path {
        lines.push(Line::from(vec![
            Span::raw("Workspace:   "),
            Span::raw(ws.clone()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("─── Status ───", Style::default().fg(Color::DarkGray))));
    lines.push(Line::from(vec![
        Span::raw("Status:      "),
        Span::styled(format!("{:?}", details.status), Style::default().fg(status_color).bold()),
    ]));
    lines.push(Line::from(vec![
        Span::raw("Created:     "),
        Span::raw(format_ts(details.created)),
    ]));

    if let Some(ts) = details.started_at {
        lines.push(Line::from(vec![
            Span::raw("Started:     "),
            Span::raw(format_ts(ts)),
        ]));
    }
    if let Some(ts) = details.finished_at {
        lines.push(Line::from(vec![
            Span::raw("Finished:    "),
            Span::raw(format_ts(ts)),
        ]));
    }
    if let Some(code) = details.exit_code {
        let color = if code == 0 { Color::Green } else { Color::Red };
        lines.push(Line::from(vec![
            Span::raw("Exit Code:   "),
            Span::styled(code.to_string(), Style::default().fg(color)),
        ]));
    }

    // Ports
    if !details.ports.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("─── Ports ───", Style::default().fg(Color::DarkGray))));
        for p in &details.ports {
            let host = p.host_port.map(|hp| hp.to_string()).unwrap_or_else(|| "-".to_string());
            lines.push(Line::from(format!(
                "  {}:{} → {}",
                host,
                p.container_port,
                p.protocol,
            )));
        }
    }

    // Mounts (all types)
    if !details.mounts.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("─── Mounts ───", Style::default().fg(Color::DarkGray))));
        for m in &details.mounts {
            let ro = if m.read_only { " (ro)" } else { "" };
            lines.push(Line::from(format!(
                "  [{}] {} → {}{}",
                m.mount_type, m.source, m.destination, ro,
            )));
        }
    }

    // Networks
    let has_network = details.network_settings.ip_address.is_some()
        || details.network_settings.gateway.is_some()
        || !details.network_settings.networks.is_empty();
    if has_network {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("─── Network ───", Style::default().fg(Color::DarkGray))));
        if let Some(ip) = &details.network_settings.ip_address {
            lines.push(Line::from(vec![
                Span::raw("IP:          "),
                Span::raw(ip.clone()),
            ]));
        }
        if let Some(gw) = &details.network_settings.gateway {
            lines.push(Line::from(vec![
                Span::raw("Gateway:     "),
                Span::raw(gw.clone()),
            ]));
        }
        let mut net_names: Vec<_> = details.network_settings.networks.keys().collect();
        net_names.sort();
        for net_name in net_names {
            let net_info = &details.network_settings.networks[net_name];
            let mut parts = vec![Span::raw(format!("  {}:", net_name))];
            if let Some(ip) = &net_info.ip_address {
                parts.push(Span::raw(format!(" {}", ip)));
            }
            if let Some(gw) = &net_info.gateway {
                parts.push(Span::styled(format!(" (gw {})", gw), Style::default().fg(Color::DarkGray)));
            }
            lines.push(Line::from(parts));
        }
    }

    // Labels
    if !details.labels.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("─── Labels ───", Style::default().fg(Color::DarkGray))));

        let well_known = [
            "devcontainer.local_folder",
            "devcontainer.config_file",
            "devc.managed",
            "devc.project",
            "devc.workspace",
            "com.docker.compose.service",
            "com.docker.compose.project",
        ];
        for key in well_known {
            if let Some(val) = details.labels.get(key) {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}: ", key), Style::default().fg(Color::Cyan)),
                    Span::raw(val.clone()),
                ]));
            }
        }

        let mut remaining: Vec<_> = details.labels.iter()
            .filter(|(k, _)| !well_known.contains(&k.as_str()) && k.as_str() != "devcontainer.metadata")
            .collect();
        remaining.sort_by_key(|(k, _)| (*k).clone());
        for (key, val) in remaining {
            lines.push(Line::from(format!("  {}: {}", key, val)));
        }
    }

    // Environment
    if !details.env.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("─── Environment ───", Style::default().fg(Color::DarkGray))));

        let skip_prefixes = [
            "PATH=", "HOME=", "HOSTNAME=", "TERM=", "LANG=", "SHELL=",
            "USER=", "SHLVL=", "PWD=", "OLDPWD=", "LC_", "LESSOPEN=",
            "LESSCLOSE=", "LS_COLORS=", "_=",
        ];
        let mut env_sorted = details.env.clone();
        env_sorted.sort();
        for var in &env_sorted {
            if !skip_prefixes.iter().any(|p| var.starts_with(p)) {
                lines.push(Line::from(format!("  {}", var)));
            }
        }
    }

    lines
}

/// Draw the discover detail popup
fn draw_discover_detail(frame: &mut Frame, app: &App, area: Rect) {
    let discovered = app.discovered_containers.get(app.selected_discovered);
    let name = discovered.map(|c| c.name.as_str()).unwrap_or("Unknown");
    let lines = match (&app.discover_detail, discovered) {
        (Some(details), Some(disc)) => build_discover_detail_text(details, disc),
        _ => vec![Line::from("Loading...")],
    };
    let detail = Paragraph::new(lines)
        .block(Block::default()
            .title(format!(" {} ", name))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)))
        .wrap(Wrap { trim: true })
        .scroll((app.discover_detail_scroll as u16, 0));
    frame.render_widget(detail, area);
}

/// Draw the compose services table within the detail popup
fn draw_compose_services(
    frame: &mut Frame,
    app: &mut App,
    container: &devc_core::ContainerState,
    area: Rect,
) {
    let services = app.compose_services.get(&container.id);

    if app.compose_services_loading && services.is_none() {
        let loading = Paragraph::new("Loading services...")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .title(" Compose Services ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        frame.render_widget(loading, area);
        return;
    }

    let services = match services {
        Some(s) if !s.is_empty() => s,
        _ => {
            let empty = Paragraph::new("No services found")
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .title(" Compose Services ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
            frame.render_widget(empty, area);
            return;
        }
    };

    let primary_service = container.compose_service.as_deref();

    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("Service"),
        Cell::from("Status"),
    ])
    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    .bottom_margin(0);

    let rows: Vec<Row> = services
        .iter()
        .map(|svc| {
            let is_primary = primary_service == Some(svc.service_name.as_str());
            let status_icon = match svc.status {
                devc_provider::ContainerStatus::Running => "●",
                devc_provider::ContainerStatus::Exited => "○",
                _ => "?",
            };
            let status_color = match svc.status {
                devc_provider::ContainerStatus::Running => Color::Green,
                devc_provider::ContainerStatus::Exited => Color::DarkGray,
                _ => Color::Yellow,
            };

            let name = if is_primary {
                format!("{} (dev)", svc.service_name)
            } else {
                svc.service_name.clone()
            };

            Row::new(vec![
                Cell::from(status_icon).style(Style::default().fg(status_color)),
                Cell::from(name).style(Style::default().bold()),
                Cell::from(svc.status.to_string()).style(Style::default().fg(status_color)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(3),   // Status icon
        Constraint::Length(18),  // Service name
        Constraint::Min(10),     // Status
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(" Compose Services ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.compose_services_table_state);
}

/// Draw build output view with scrolling
fn draw_build_output(frame: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let total_lines = app.build_output.len();

    // Calculate effective scroll position
    // When auto_scroll is enabled, always show the latest lines
    let scroll = if app.build_auto_scroll {
        // Scroll to show last lines, but don't go negative
        total_lines.saturating_sub(inner_height)
    } else {
        app.build_output_scroll
    };

    // Build text lines using ansi-to-tui for proper ANSI handling
    let text: Vec<Line> = app
        .build_output
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner_height)
        .map(|(i, line)| {
            // Replace carriage returns with nothing (they cause in-place overwrites)
            // ansi-to-tui handles ANSI escape sequences properly
            let clean_line = line.replace('\r', "");

            // Convert ANSI to ratatui spans, preserving colors
            let line_num = Span::styled(
                format!("{:>4} ", i + 1),
                Style::default().fg(Color::DarkGray),
            );

            // Use ansi-to-tui to parse the line content
            match clean_line.into_text() {
                Ok(text) => {
                    // Combine line number with parsed content
                    let mut spans = vec![line_num];
                    if let Some(first_line) = text.lines.into_iter().next() {
                        spans.extend(first_line.spans);
                    }
                    Line::from(spans)
                }
                Err(_) => {
                    // Fallback to raw text if parsing fails
                    Line::from(vec![line_num, Span::raw(clean_line)])
                }
            }
        })
        .collect();

    let title = if app.build_complete {
        if total_lines > 0 {
            format!(
                " Build Output [{}/{}] - Press q to close ",
                scroll + 1,
                total_lines
            )
        } else {
            " Build Output - Press q to close ".to_string()
        }
    } else if total_lines > 0 {
        format!(
            " Build Output [{}/{}] - Building... ",
            scroll + 1,
            total_lines
        )
    } else {
        " Build Output - Building... ".to_string()
    };

    let border_color = if app.build_complete {
        Color::Green
    } else {
        Color::Yellow
    };

    let output = Paragraph::new(text).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );

    frame.render_widget(output, area);

    // Render scrollbar if content exceeds visible area
    if total_lines > inner_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));
        let mut scrollbar_state =
            ScrollbarState::new(total_lines.saturating_sub(inner_height)).position(scroll);

        // Render scrollbar in a slightly inset area
        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

/// Draw logs view with scrolling
fn draw_logs(frame: &mut Frame, app: &App, area: Rect) {
    let container_name = app
        .selected_container()
        .map(|c| c.name.as_str())
        .unwrap_or("Unknown");
    let display_name = if let Some(ref svc_name) = app.logs_service_name {
        format!("{}/{}", container_name, svc_name)
    } else {
        container_name.to_string()
    };

    let inner_height = area.height.saturating_sub(2) as usize;
    let total_lines = app.logs.len();

    let text: Vec<Line> = app
        .logs
        .iter()
        .enumerate()
        .skip(app.logs_scroll)
        .take(inner_height)
        .map(|(i, line)| {
            Line::from(vec![
                Span::styled(
                    format!("{:>5} ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(line.as_str()),
            ])
        })
        .collect();

    let scroll_info = if total_lines > 0 {
        let percent = if total_lines <= inner_height {
            100
        } else {
            ((app.logs_scroll + inner_height).min(total_lines) * 100) / total_lines
        };
        format!(
            " Logs: {} [{}/{}] {}% ",
            display_name,
            app.logs_scroll + 1,
            total_lines,
            percent
        )
    } else {
        format!(" Logs: {} (empty) ", display_name)
    };

    let logs = Paragraph::new(text).block(
        Block::default()
            .title(scroll_info)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    frame.render_widget(logs, area);

    // Render scrollbar if content exceeds visible area
    if total_lines > inner_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));
        let mut scrollbar_state = ScrollbarState::new(total_lines.saturating_sub(inner_height))
            .position(app.logs_scroll);

        // Render scrollbar in a slightly inset area
        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

/// Draw port forwarding view
fn draw_ports(frame: &mut Frame, app: &mut App, area: Rect) {
    let container_name = app
        .containers
        .iter()
        .find(|c| Some(&c.id) == app.ports_container_id.as_ref())
        .map(|c| c.name.as_str())
        .unwrap_or("Unknown");

    // Show socat warning if not installed
    let socat_warning = match (app.socat_installed, app.socat_installing) {
        (_, true) => Some(("Installing socat...", Color::Yellow)),
        (Some(false), _) => Some(("⚠ socat not installed - press 'i' to install", Color::Yellow)),
        _ => None,
    };

    if app.detected_ports.is_empty() {
        let message = if let Some((warning, _)) = socat_warning {
            format!("{}\n\nNo ports detected.\n\nWaiting for port detection...", warning)
        } else {
            "No ports detected.\n\nWaiting for port detection...".to_string()
        };

        let empty = Paragraph::new(message)
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .title(format!(" Port Forwarding: {} ", container_name))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(empty, area);
        return;
    }

    // Build table rows
    let container_id_for_auto = app.ports_provider_container_id.clone();
    let auto_configs = container_id_for_auto
        .as_ref()
        .and_then(|cid| app.auto_forward_configs.get(cid));
    let rows: Vec<Row> = app
        .detected_ports
        .iter()
        .map(|port| {
            let is_auto = container_id_for_auto
                .as_ref()
                .map(|cid| app.auto_forwarded_ports.contains(&(cid.clone(), port.port)))
                .unwrap_or(false);
            let status = if port.is_forwarded && is_auto {
                "● Forwarded [auto]"
            } else if port.is_forwarded {
                "● Forwarded"
            } else {
                "○ Detected"
            };
            let local = if port.is_forwarded {
                format!("localhost:{}", port.port)
            } else {
                "-".to_string()
            };
            let new_marker = if port.is_new { " [NEW]" } else { "" };
            let process = port.process.as_deref().unwrap_or("-");

            // Look up label from auto_forward_configs
            let label = auto_configs.and_then(|configs| {
                configs.iter().find(|c| c.port == port.port).and_then(|c| c.label.as_deref())
            });
            let port_cell = if let Some(label) = label {
                format!("{} ({})", port.port, label)
            } else {
                port.port.to_string()
            };

            let style = if port.is_forwarded {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(port_cell),
                Cell::from(status),
                Cell::from(local),
                Cell::from(format!("{}{}", process, new_marker)),
            ])
            .style(style)
        })
        .collect();

    let header = Row::new(vec![
        Cell::from("PORT"),
        Cell::from("STATUS"),
        Cell::from("LOCAL"),
        Cell::from("PROCESS"),
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1);

    let widths = [
        Constraint::Length(20),
        Constraint::Length(20),
        Constraint::Length(18),
        Constraint::Min(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(format!(" Port Forwarding: {} ", container_name))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.ports_table_state);
}

/// Draw install progress modal with spinner
fn draw_install_progress(frame: &mut Frame, app: &App, area: Rect) {
    const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spinner = SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()];

    DialogBuilder::new("Installing")
        .width(40)
        .border_color(Color::Yellow)
        .empty_line()
        .styled_message(Line::from(vec![
            Span::styled(spinner, Style::default().fg(Color::Cyan)),
            Span::raw(" Installing socat..."),
        ]))
        .empty_line()
        .help("Ctrl+C or Esc to cancel")
        .render(frame, area);
}

/// Draw container operation progress modal with spinner
fn draw_operation_progress(frame: &mut Frame, app: &App, area: Rect) {
    const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spinner = SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()];

    let op = match &app.container_op {
        Some(op) => op,
        None => return,
    };

    let title = match op {
        ContainerOperation::Starting { .. } => "Starting",
        ContainerOperation::Stopping { .. } => "Stopping",
        ContainerOperation::Deleting { .. } => "Deleting",
        ContainerOperation::Up { .. } => "Container Up",
    };

    let has_output = !app.up_output.is_empty();
    let dialog_width: u16 = if has_output { 60 } else { 40 };
    let max_output_lines: usize = 12;

    let mut builder = DialogBuilder::new(title)
        .width(dialog_width)
        .border_color(Color::Yellow)
        .empty_line()
        .styled_message(Line::from(vec![
            Span::styled(spinner, Style::default().fg(Color::Cyan)),
            Span::raw(format!(" {}", op.label())),
        ]));

    if has_output {
        builder = builder.empty_line();
        let total = app.up_output.len();
        let skip = total.saturating_sub(max_output_lines);
        if skip > 0 {
            builder = builder.styled_message(Line::from(Span::styled(
                format!("  ... ({} total lines)", total),
                Style::default().fg(Color::DarkGray),
            )));
        }
        let inner_width = (dialog_width - 4) as usize; // borders + padding
        for line in app.up_output.iter().skip(skip) {
            let truncated = if line.len() > inner_width {
                format!("{}...", &line[..inner_width - 3])
            } else {
                line.clone()
            };
            builder = builder.styled_message(Line::from(Span::styled(
                format!("  {}", truncated),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    builder
        .empty_line()
        .help("Esc to dismiss")
        .render(frame, area);
}

/// Draw help view - context-sensitive based on current tab
fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let general_help = vec![
        Line::from(""),
        Line::from(Span::styled("Global Keys", Style::default().bold().underlined())),
        Line::from(""),
        Line::from("  Tab         Next tab"),
        Line::from("  Shift+Tab   Previous tab"),
        Line::from("  1/2/3       Jump to Containers/Providers/Settings tab"),
        Line::from("  ?/F1        Show this help"),
        Line::from("  q           Quit (or go back from subview)"),
        Line::from("  Esc         Go back / Cancel"),
        Line::from(""),
    ];

    let tab_help = match app.tab {
        Tab::Containers => vec![
            Line::from(Span::styled("Containers Tab", Style::default().bold().underlined())),
            Line::from(""),
            Line::from("  j/Down      Move selection down"),
            Line::from("  k/Up        Move selection up"),
            Line::from("  g/Home      Go to first container"),
            Line::from("  G/End       Go to last container"),
            Line::from("  Enter       View container details"),
            Line::from(""),
            Line::from("  s           Start or Stop container"),
            Line::from("  u           Up - build, create, and start"),
            Line::from("  S           Shell (persistent session, Ctrl+\\ to detach)"),
            Line::from("  R           Rebuild - destroy and rebuild container"),
            Line::from("  p           Port forwarding"),
            Line::from("  d/Delete    Delete container"),
            Line::from("  r/F5        Refresh list"),
        ],
        Tab::Providers => vec![
            Line::from(Span::styled("Providers Tab", Style::default().bold().underlined())),
            Line::from(""),
            Line::from("  j/Down      Move selection down"),
            Line::from("  k/Up        Move selection up"),
            Line::from("  Enter       Set selected provider as active"),
            Line::from("  Space       Set selected provider as active"),
            Line::from("  s           Save provider settings to config"),
            Line::from(""),
            Line::from("  The active provider is used for new containers."),
            Line::from("  Existing containers keep their original provider."),
        ],
        Tab::Settings => vec![
            Line::from(Span::styled("Settings Tab", Style::default().bold().underlined())),
            Line::from(""),
            Line::from("  j/Down      Move to next setting"),
            Line::from("  k/Up        Move to previous setting"),
            Line::from("  Enter       Edit setting (text) or toggle (checkbox)"),
            Line::from("  Space       Edit setting (text) or toggle (checkbox)"),
            Line::from("  s           Save all settings to config file"),
            Line::from("  r           Reset to saved values"),
            Line::from(""),
            Line::from("  When editing text:"),
            Line::from("    Enter     Confirm change"),
            Line::from("    Esc       Cancel change"),
        ],
    };

    let mut text = general_help;
    text.extend(tab_help);

    let help = Paragraph::new(text)
        .block(Block::default().title(" Help ").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(help, area);
}

/// Draw confirmation dialog
fn draw_confirm_dialog(frame: &mut Frame, app: &App, area: Rect) {
    match &app.confirm_action {
        Some(ConfirmAction::Delete(id)) => {
            let container = app.containers.iter().find(|c| &c.id == id);
            let name = container.map(|c| c.name.as_str()).unwrap_or(id);
            let is_adopted = container.map(|c| c.source != DevcontainerSource::Devc).unwrap_or(false);
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
        Some(ConfirmAction::Rebuild { id, provider_change }) => {
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
            draw_simple_confirm_dialog(
                frame,
                app,
                area,
                "Cancel build in progress?",
            );
        }
        Some(ConfirmAction::QuitApp) => {
            draw_simple_confirm_dialog(
                frame,
                app,
                area,
                "Quit devc?",
            );
        }
        None => {}
    }
}

/// Draw a simple yes/no confirmation dialog
fn draw_simple_confirm_dialog(frame: &mut Frame, app: &App, area: Rect, message: &str) {
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
fn draw_set_provider_confirm_dialog(frame: &mut Frame, app: &App, area: Rect, provider_name: &str) {
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
fn draw_rebuild_confirm_dialog(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    name: &str,
    provider_change: Option<&(devc_provider::ProviderType, devc_provider::ProviderType)>,
) {
    // Pre-format strings to avoid lifetime issues
    let message = format!("Rebuild '{}'?", name);
    let warning_text = provider_change
        .map(|(old, new)| format!("{} -> {}", old, new));

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

