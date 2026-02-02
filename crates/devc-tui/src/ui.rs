//! UI rendering for the TUI application

use ansi_to_tui::IntoText;
use crate::app::{App, ConfirmAction, DialogFocus, Tab, View};
use crate::settings::{SettingsField, SettingsSection};
use crate::widgets::DialogBuilder;
use devc_core::DevcContainerStatus;
use devc_provider::{ContainerStatus, DevcontainerSource};
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Cell, List, ListItem, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, Tabs, Wrap,
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
        View::Main => match app.tab {
            Tab::Containers => {
                if app.discover_mode {
                    draw_discovered_containers(frame, app, content_area);
                } else {
                    draw_containers(frame, app, content_area);
                }
            }
            Tab::Providers => draw_providers(frame, app, content_area),
            Tab::Settings => draw_settings(frame, app, content_area),
        },
        View::ContainerDetail => draw_detail(frame, app, content_area),
        View::ProviderDetail => draw_provider_detail(frame, app, content_area),
        View::BuildOutput => draw_build_output(frame, app, content_area),
        View::Logs => draw_logs(frame, app, content_area),
        View::Help => draw_help(frame, app, content_area),
        View::Confirm => {
            // Draw the main content behind the dialog
            match app.tab {
                Tab::Containers => {
                    if app.discover_mode {
                        draw_discovered_containers(frame, app, content_area);
                    } else {
                        draw_containers(frame, app, content_area);
                    }
                }
                Tab::Providers => draw_providers(frame, app, content_area),
                Tab::Settings => draw_settings(frame, app, content_area),
            }
            draw_confirm_dialog(frame, app, area);
        }
    }

    draw_footer(frame, app, footer_area);
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
            let label = tab.label();
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

/// Draw the footer with context-sensitive help
fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.view {
        View::Main => match app.tab {
            Tab::Containers => {
                if app.discover_mode {
                    "Esc/q: Exit  j/k: Navigate  a: Adopt  r: Refresh  ?: Help"
                } else {
                    "D: Discover  j/k: Navigate  Enter: Details  b: Build  s: Start/Stop  u: Up  R: Rebuild  d: Delete  ?: Help  q: Quit"
                }
            }
            Tab::Providers => {
                "Tab/1-3: Switch tabs  j/k: Navigate  Enter: Configure  Space/a: Set Active  s: Save  ?: Help  q: Quit"
            }
            Tab::Settings => {
                if app.settings_state.editing {
                    "Enter: Confirm  Esc: Cancel  Type to edit"
                } else {
                    "Tab/1-3: Switch tabs  j/k: Navigate  Enter/Space: Edit  s: Save  r: Reset  ?: Help  q: Quit"
                }
            }
        },
        View::ContainerDetail => "b: Build  s: Start/Stop  u: Up  R: Rebuild  l: Logs  Esc/q: Back  ?: Help",
        View::ProviderDetail => {
            if app.provider_detail_state.editing {
                "Enter: Confirm  Esc: Cancel  Type to edit"
            } else {
                "e: Edit Socket  t: Test Connection  a/Space: Set Active  s: Save  Esc/q: Back"
            }
        }
        View::BuildOutput => {
            if app.build_complete {
                "j/k: Scroll  g/G: Top/Bottom  c: Copy  q/Esc: Close"
            } else {
                "j/k: Scroll  g/G: Top/Bottom  c: Copy  (building...)"
            }
        }
        View::Logs => "j/k: Scroll  g/G: Top/Bottom  PgUp/PgDn: Page  r: Refresh  Esc/q: Back",
        View::Help => "Press any key to close",
        View::Confirm => {
            if matches!(app.confirm_action, Some(ConfirmAction::Rebuild { .. })) {
                "y/Enter: Confirm  n/Esc: Cancel  Space: Toggle no-cache"
            } else {
                "y/Enter: Yes  n/Esc: No"
            }
        }
    };

    let status = app.status_message.as_deref().unwrap_or("");

    let footer_text = if status.is_empty() {
        help_text.to_string()
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
                DevcContainerStatus::Running => "●",
                DevcContainerStatus::Stopped => "○",
                DevcContainerStatus::Building => "◐",
                DevcContainerStatus::Built => "◑",
                DevcContainerStatus::Created => "◔",
                DevcContainerStatus::Failed => "✗",
                DevcContainerStatus::Configured => "◯",
            };

            let status_color = match container.status {
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

            Row::new(vec![
                Cell::from(status_symbol).style(Style::default().fg(status_color)),
                Cell::from(container.name.clone()).style(Style::default().bold()),
                Cell::from(format!("{}", container.status)).style(Style::default().fg(status_color)),
                Cell::from(format!("{}", container.provider)),
                Cell::from(workspace_display).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Define column widths
    let widths = [
        Constraint::Length(3),   // Status icon
        Constraint::Length(22),  // Name
        Constraint::Length(12),  // Status
        Constraint::Length(10),  // Provider
        Constraint::Min(20),     // Workspace (takes remaining)
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
        Cell::from("Source"),
        Cell::from("Managed"),
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
                DevcontainerSource::Other => "other",
            };

            let managed_str = if container.managed { "Yes" } else { "No" };
            let managed_color = if container.managed { Color::Green } else { Color::Yellow };

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

            Row::new(vec![
                Cell::from(status_symbol).style(Style::default().fg(status_color)),
                Cell::from(name_display).style(Style::default().bold()),
                Cell::from(format!("{}", container.status)).style(Style::default().fg(status_color)),
                Cell::from(source_str).style(Style::default().fg(Color::Cyan)),
                Cell::from(managed_str).style(Style::default().fg(managed_color)),
                Cell::from(workspace_display).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Define column widths
    let widths = [
        Constraint::Length(3),   // Status icon
        Constraint::Length(22),  // Name
        Constraint::Length(10),  // Status
        Constraint::Length(8),   // Source
        Constraint::Length(8),   // Managed
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
                Cell::from(provider.name.clone()),
                Cell::from(Span::styled(status_text, status_style)),
                Cell::from(Span::styled(
                    provider.socket.clone(),
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
            let label = field.label();
            let value = settings.draft.get_value(field);

            let display_value = if settings.editing && is_focused {
                // Show edit buffer with cursor
                let cursor_pos = settings.cursor();
                let before = &settings.edit_buffer()[..cursor_pos];
                let after = &settings.edit_buffer()[cursor_pos..];
                format!("{}│{}", before, after)
            } else if field.is_toggle() {
                if let SettingsField::SshEnabled = field {
                    if value == "true" {
                        "[●] Enabled  [ ] Disabled".to_string()
                    } else {
                        "[ ] Enabled  [●] Disabled".to_string()
                    }
                } else {
                    value.clone()
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
                Span::styled(format!("   {:<16}", label), style.bold()),
                Span::styled(display_value, style),
            ]);

            items.push(ListItem::new(line).style(style));
            field_index += 1;
        }
    }

    let title = if settings.dirty {
        " Settings (unsaved changes) "
    } else {
        " Settings "
    };

    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(if settings.dirty {
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

/// Draw the container detail view
fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let container = match app.selected_container() {
        Some(c) => c,
        None => return,
    };

    let status_color = match container.status {
        DevcContainerStatus::Running => Color::Green,
        DevcContainerStatus::Stopped => Color::DarkGray,
        DevcContainerStatus::Building => Color::Yellow,
        DevcContainerStatus::Built => Color::Blue,
        DevcContainerStatus::Created => Color::Cyan,
        DevcContainerStatus::Failed => Color::Red,
        DevcContainerStatus::Configured => Color::DarkGray,
    };

    let text = vec![
        Line::from(vec![
            Span::raw("Name:        "),
            Span::styled(&container.name, Style::default().bold()),
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
            Span::raw("ID:          "),
            Span::styled(&container.id, Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Workspace:   "),
            Span::raw(container.workspace_path.to_string_lossy().to_string()),
        ]),
        Line::from(vec![
            Span::raw("Config:      "),
            Span::raw(container.config_path.to_string_lossy().to_string()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Image ID:    "),
            Span::raw(container.image_id.as_deref().unwrap_or("Not built")),
        ]),
        Line::from(vec![
            Span::raw("Container:   "),
            Span::raw(container.container_id.as_deref().unwrap_or("Not created")),
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

    let detail = Paragraph::new(text)
        .block(
            Block::default()
                .title(format!(" {} ", container.name))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, area);
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
            container_name,
            app.logs_scroll + 1,
            total_lines,
            percent
        )
    } else {
        format!(" Logs: {} (empty) ", container_name)
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
            Line::from("  b           Build container image"),
            Line::from("  s           Start or Stop container"),
            Line::from("  u           Up - build, create, and start"),
            Line::from("  R           Rebuild - destroy and rebuild container"),
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
            let name = app
                .containers
                .iter()
                .find(|c| &c.id == id)
                .map(|c| c.name.as_str())
                .unwrap_or(id);
            draw_simple_confirm_dialog(frame, app, area, &format!("Delete container '{}'?", name));
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
        None => {}
    }
}

/// Draw a simple yes/no confirmation dialog
fn draw_simple_confirm_dialog(frame: &mut Frame, app: &App, area: Rect, message: &str) {
    DialogBuilder::new("Confirm")
        .width(50)
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

