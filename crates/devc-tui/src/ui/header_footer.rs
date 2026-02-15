use super::*;

pub(super) fn draw_header_with_tabs(frame: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn container_list_footer(app: &App) -> String {
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
pub(super) fn container_detail_footer(app: &App) -> String {
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
pub(super) fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
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

/// Draw the providers tab
pub(super) fn draw_providers(frame: &mut Frame, app: &mut App, area: Rect) {
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
pub(super) fn draw_settings(frame: &mut Frame, app: &App, area: Rect) {
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

pub(super) fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
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
