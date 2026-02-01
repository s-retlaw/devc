//! UI rendering for the TUI application

use ansi_to_tui::IntoText;
use crate::app::{App, ConfirmAction, DialogFocus, Tab, View};
use crate::settings::{SettingsField, SettingsSection};
use devc_core::DevcContainerStatus;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap},
};

/// Main draw function
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.size();

    // Main layout: header with tabs, content, footer with help
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header with tabs
            Constraint::Min(0),    // Content
            Constraint::Length(3), // Footer
        ])
        .split(area);

    draw_header_with_tabs(frame, app, chunks[0]);

    match app.view {
        View::Main => match app.tab {
            Tab::Containers => draw_containers(frame, app, chunks[1]),
            Tab::Providers => draw_providers(frame, app, chunks[1]),
            Tab::Settings => draw_settings(frame, app, chunks[1]),
        },
        View::ContainerDetail => draw_detail(frame, app, chunks[1]),
        View::ProviderDetail => draw_provider_detail(frame, app, chunks[1]),
        View::BuildOutput => draw_build_output(frame, app, chunks[1]),
        View::Logs => draw_logs(frame, app, chunks[1]),
        View::Help => draw_help(frame, app, chunks[1]),
        View::Confirm => {
            // Draw the main content behind the dialog
            match app.tab {
                Tab::Containers => draw_containers(frame, app, chunks[1]),
                Tab::Providers => draw_providers(frame, app, chunks[1]),
                Tab::Settings => draw_settings(frame, app, chunks[1]),
            }
            draw_confirm_dialog(frame, app, area);
        }
    }

    draw_footer(frame, app, chunks[2]);
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
                "Tab/1-3: Switch tabs  j/k: Navigate  Enter: Details  b: Build  s: Start/Stop  u: Up  R: Rebuild  d: Delete  ?: Help  q: Quit"
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
                "j/k: Scroll  g/G: Top/Bottom  q/Esc: Close"
            } else {
                "j/k: Scroll  g/G: Top/Bottom  (building...)"
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

/// Draw the containers tab
fn draw_containers(frame: &mut Frame, app: &App, area: Rect) {
    if app.containers.is_empty() {
        let empty = Paragraph::new(
            "No containers found.\n\n\
             Use 'devc init' in a directory with devcontainer.json to add a container.\n\n\
             Or use 'devc list --discover' to find VS Code devcontainers.",
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

    let items: Vec<ListItem> = app
        .containers
        .iter()
        .enumerate()
        .map(|(i, container)| {
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

            let is_selected = i == app.selected;
            let style = if is_selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };

            let line = Line::from(vec![
                Span::styled(
                    format!(" {} ", status_symbol),
                    Style::default().fg(status_color),
                ),
                Span::styled(format!("{:<20}", container.name), style.bold()),
                Span::styled(format!("{:<12}", container.status), style.fg(status_color)),
                Span::styled(
                    format!("{:<10}", container.provider),
                    style.fg(Color::DarkGray),
                ),
                Span::styled(format_time_ago(container.last_used.timestamp()), style.fg(Color::DarkGray)),
            ]);

            ListItem::new(line).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Containers ")
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));

    frame.render_widget(list, area);
}

/// Draw the providers tab
fn draw_providers(frame: &mut Frame, app: &App, area: Rect) {
    let mut items: Vec<ListItem> = Vec::new();

    // Add a header explanation
    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            " Select a provider to use for new containers:",
            Style::default().fg(Color::DarkGray).italic(),
        ),
    ])));
    items.push(ListItem::new(Line::from("")));

    for (i, provider) in app.providers.iter().enumerate() {
        let is_selected = i == app.selected_provider;

        let active_indicator = if provider.is_active {
            "● ACTIVE"
        } else {
            "○       "
        };

        let status_indicator = if provider.connected {
            Span::styled("Connected", Style::default().fg(Color::Green))
        } else {
            Span::styled("Not connected", Style::default().fg(Color::Red))
        };

        let style = if is_selected {
            Style::default().bg(Color::DarkGray).fg(Color::White)
        } else {
            Style::default()
        };

        // Provider name and active status
        let line1 = Line::from(vec![
            Span::styled(
                format!(" {} ", active_indicator),
                if provider.is_active {
                    Style::default().fg(Color::Green).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::styled(format!("{:<10}", provider.name), style.bold()),
            status_indicator,
        ]);

        // Socket path
        let line2 = Line::from(vec![
            Span::raw("          "),
            Span::styled(
                format!("Socket: {}", provider.socket),
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        items.push(ListItem::new(vec![line1, line2]).style(style));
        items.push(ListItem::new(Line::from("")));
    }

    let list = List::new(items).block(
        Block::default()
            .title(" Providers - Container Runtimes ")
            .borders(Borders::ALL),
    );

    frame.render_widget(list, area);
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
                let cursor_pos = settings.cursor;
                let before = &settings.edit_buffer[..cursor_pos];
                let after = &settings.edit_buffer[cursor_pos..];
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
        let cursor_pos = detail_state.cursor;
        let before = &detail_state.edit_buffer[..cursor_pos];
        let after = &detail_state.edit_buffer[cursor_pos..];
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
        None => {}
    }
}

/// Draw a simple yes/no confirmation dialog
fn draw_simple_confirm_dialog(frame: &mut Frame, app: &App, area: Rect, message: &str) {
    let dialog_width = 50;
    let dialog_height = 8;
    let dialog_area = Rect {
        x: (area.width.saturating_sub(dialog_width)) / 2,
        y: (area.height.saturating_sub(dialog_height)) / 2,
        width: dialog_width.min(area.width),
        height: dialog_height.min(area.height),
    };

    frame.render_widget(Clear, dialog_area);

    // Button styles based on focus
    let confirm_style = if app.dialog_focus == DialogFocus::Confirm {
        Style::default().bg(Color::Green).fg(Color::Black).bold()
    } else {
        Style::default().fg(Color::Green)
    };
    let cancel_style = if app.dialog_focus == DialogFocus::Cancel {
        Style::default().bg(Color::Red).fg(Color::White).bold()
    } else {
        Style::default().fg(Color::Red)
    };

    let dialog = Paragraph::new(vec![
        Line::from(""),
        Line::from(message.to_string()),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Confirm  ", confirm_style),
            Span::raw("    "),
            Span::styled("  Cancel  ", cancel_style),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Tab: Switch  Enter: Select  Esc: Cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .title(" Confirm ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );

    frame.render_widget(dialog, dialog_area);
}

/// Draw the rebuild confirmation dialog with provider change warning and no-cache toggle
fn draw_rebuild_confirm_dialog(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    name: &str,
    provider_change: Option<&(devc_provider::ProviderType, devc_provider::ProviderType)>,
) {
    let dialog_width = 50;
    let dialog_height = if provider_change.is_some() { 13 } else { 11 };
    let dialog_area = Rect {
        x: (area.width.saturating_sub(dialog_width)) / 2,
        y: (area.height.saturating_sub(dialog_height)) / 2,
        width: dialog_width.min(area.width),
        height: dialog_height.min(area.height),
    };

    frame.render_widget(Clear, dialog_area);

    // Styles based on focus state
    let checkbox_style = if app.dialog_focus == DialogFocus::Checkbox {
        Style::default().bg(Color::Cyan).fg(Color::Black).bold()
    } else {
        Style::default().fg(Color::Cyan)
    };
    let confirm_style = if app.dialog_focus == DialogFocus::Confirm {
        Style::default().bg(Color::Green).fg(Color::Black).bold()
    } else {
        Style::default().fg(Color::Green)
    };
    let cancel_style = if app.dialog_focus == DialogFocus::Cancel {
        Style::default().bg(Color::Red).fg(Color::White).bold()
    } else {
        Style::default().fg(Color::Red)
    };

    let mut lines = vec![
        Line::from(""),
        Line::from(format!("Rebuild '{}'?", name)),
        Line::from(""),
    ];

    // Add provider change warning if applicable
    if let Some((old, new)) = provider_change {
        lines.push(Line::from(vec![
            Span::styled("  Warning: ", Style::default().fg(Color::Yellow).bold()),
            Span::styled(format!("{} -> {}", old, new), Style::default().fg(Color::Yellow)),
        ]));
        lines.push(Line::from(""));
    }

    // Add no-cache checkbox with focus indicator
    let checkbox = if app.rebuild_no_cache { "[X]" } else { "[ ]" };
    let focus_indicator = if app.dialog_focus == DialogFocus::Checkbox { "▶ " } else { "  " };
    lines.push(Line::from(vec![
        Span::raw(focus_indicator),
        Span::styled(checkbox, checkbox_style),
        Span::styled(" Force rebuild (no cache)", if app.dialog_focus == DialogFocus::Checkbox {
            Style::default().bold()
        } else {
            Style::default()
        }),
    ]));
    lines.push(Line::from(""));

    // Add button row with focus indicators
    lines.push(Line::from(vec![
        Span::styled("  Confirm  ", confirm_style),
        Span::raw("    "),
        Span::styled("  Cancel  ", cancel_style),
    ]));
    lines.push(Line::from(""));

    // Help text
    lines.push(Line::from(Span::styled(
        "Tab: Switch  Enter/Space: Select  Esc: Cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let dialog = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(" Rebuild Container ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );

    frame.render_widget(dialog, dialog_area);
}

/// Format a timestamp as "X ago"
fn format_time_ago(timestamp: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - timestamp;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 604800 {
        format!("{}d ago", diff / 86400)
    } else {
        format!("{}w ago", diff / 604800)
    }
}
