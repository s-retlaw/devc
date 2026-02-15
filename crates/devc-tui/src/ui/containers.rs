use super::*;

pub(super) fn draw_containers(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.containers.is_empty() {
        let empty = Paragraph::new(
            "No containers found.\n\n\
             Use 'devc init' in a directory with devcontainer.json to add a container.\n\n\
             Press 'D' to discover existing devcontainers.",
        )
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().title(" Containers ").borders(Borders::ALL))
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
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
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
                format!("...{}", &workspace[workspace.len() - 32..])
            } else {
                workspace
            };

            // Show [S] indicator if there's an active shell session for this container
            let has_shell = app.shell_state.shell_sessions.contains_key(&container.id);
            let name_display = if has_shell {
                format!("{} [S]", container.name)
            } else if container.compose_project.is_some() {
                let suffix = match app.compose_state.services.get(&container.id) {
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
                Cell::from(container.source.to_string())
                    .style(Style::default().fg(Color::DarkGray)),
                Cell::from(container.status.to_string()).style(Style::default().fg(status_color)),
                Cell::from(container.provider.to_string()),
                Cell::from(workspace_display).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Define column widths
    let widths = [
        Constraint::Length(3),  // Status icon
        Constraint::Length(24), // Name
        Constraint::Length(8),  // Source
        Constraint::Length(12), // Status
        Constraint::Length(8),  // Provider
        Constraint::Min(10),    // Workspace (takes remaining)
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().title(" Containers ").borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.containers_table_state);
}

/// Draw discovered containers using Table widget with headers
pub(super) fn draw_discovered_containers(frame: &mut Frame, app: &mut App, area: Rect) {
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
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
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
                format!("...{}", &workspace[workspace.len() - 27..])
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
                Cell::from(format!("{}", container.status))
                    .style(Style::default().fg(status_color)),
                Cell::from(provider_str).style(Style::default().fg(Color::Blue)),
                Cell::from(source_str).style(Style::default().fg(Color::Cyan)),
                Cell::from(workspace_display).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Define column widths
    let widths = [
        Constraint::Length(3),  // Status icon
        Constraint::Length(22), // Name
        Constraint::Length(10), // Status
        Constraint::Length(8),  // Provider
        Constraint::Length(8),  // Source
        Constraint::Min(20),    // Workspace (takes remaining)
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
