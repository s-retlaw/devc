use super::*;

pub(super) fn draw_ports(frame: &mut Frame, app: &mut App, area: Rect) {
    let container_name = app
        .containers
        .iter()
        .find(|c| Some(&c.id) == app.port_state.container_id.as_ref())
        .map(|c| c.name.as_str())
        .unwrap_or("Unknown");

    // Show socat warning if not installed
    let socat_warning = match (
        app.port_state.socat_installed,
        app.port_state.socat_installing,
    ) {
        (_, true) => Some(("Installing socat...", Color::Yellow)),
        (Some(false), _) => Some((
            "⚠ socat not installed - press 'i' to install",
            Color::Yellow,
        )),
        _ => None,
    };

    if app.port_state.detected_ports.is_empty() {
        let message = if let Some((warning, _)) = socat_warning {
            format!(
                "{}\n\nNo ports detected.\n\nWaiting for port detection...",
                warning
            )
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
    let container_id_for_auto = app.port_state.provider_container_id.clone();
    let auto_configs = container_id_for_auto
        .as_ref()
        .and_then(|cid| app.port_state.auto_forward_configs.get(cid));
    let rows: Vec<Row> = app
        .port_state
        .detected_ports
        .iter()
        .map(|port| {
            let is_auto = container_id_for_auto
                .as_ref()
                .map(|cid| {
                    app.port_state
                        .auto_forwarded_ports
                        .contains(&(cid.clone(), port.port))
                })
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
                configs
                    .iter()
                    .find(|c| c.port == port.port)
                    .and_then(|c| c.label.as_deref())
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

    frame.render_stateful_widget(table, area, &mut app.port_state.table_state);
}
