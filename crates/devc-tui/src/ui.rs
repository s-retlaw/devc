//! UI rendering for the TUI application

use crate::app::{App, ConfirmAction, View};
use devc_core::DevcContainerStatus;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

/// Main draw function
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.size();

    // Main layout: header, content, footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Content
            Constraint::Length(3), // Footer
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);

    match app.view {
        View::Dashboard => draw_dashboard(frame, app, chunks[1]),
        View::ContainerDetail => draw_detail(frame, app, chunks[1]),
        View::BuildOutput => draw_build_output(frame, app, chunks[1]),
        View::Logs => draw_logs(frame, app, chunks[1]),
        View::Help => draw_help(frame, chunks[1]),
        View::Confirm => {
            draw_dashboard(frame, app, chunks[1]);
            draw_confirm_dialog(frame, app, area);
        }
    }

    draw_footer(frame, app, chunks[2]);
}

/// Draw the header
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(
        " devc - Dev Container Manager  [{}] ",
        app.manager.provider_type()
    );

    let header = Paragraph::new(title)
        .style(Style::default().fg(Color::Cyan).bold())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );

    frame.render_widget(header, area);
}

/// Draw the footer with status and help
fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.view {
        View::Dashboard => "[j/k] Navigate  [b]uild  [s]tart/stop  [u]p  [d]elete  [?] Help  [q]uit",
        View::ContainerDetail => "[b]uild  [s]tart/stop  [u]p  [l]ogs  [q] Back",
        View::BuildOutput => "[q] Back",
        View::Logs => "[j/k] Scroll  [g/G] Top/Bottom  [C-d/C-u] Page  [r]efresh  [q] Back",
        View::Help => "Press any key to close",
        View::Confirm => "[y]es  [n]o",
    };

    let status = app
        .status_message
        .as_deref()
        .unwrap_or("");

    let footer_text = if status.is_empty() {
        help_text.to_string()
    } else {
        format!("{} | {}", status, help_text)
    };

    let footer = Paragraph::new(footer_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));

    frame.render_widget(footer, area);
}

/// Draw the dashboard view
fn draw_dashboard(frame: &mut Frame, app: &App, area: Rect) {
    if app.containers.is_empty() {
        let empty = Paragraph::new("No containers found.\n\nUse 'devc init' in a directory with devcontainer.json to add a container.")
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

            let style = if i == app.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };

            let line = Line::from(vec![
                Span::styled(format!(" {} ", status_symbol), Style::default().fg(status_color)),
                Span::styled(
                    format!("{:<20}", container.name),
                    style.bold(),
                ),
                Span::styled(
                    format!("{:<12}", container.status),
                    style.fg(status_color),
                ),
                Span::styled(
                    format!("{:<10}", container.provider),
                    style.fg(Color::DarkGray),
                ),
                Span::styled(
                    format_time_ago(container.last_used.timestamp()),
                    style.fg(Color::DarkGray),
                ),
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
            Span::raw(
                container
                    .image_id
                    .as_deref()
                    .unwrap_or("Not built"),
            ),
        ]),
        Line::from(vec![
            Span::raw("Container:   "),
            Span::raw(
                container
                    .container_id
                    .as_deref()
                    .unwrap_or("Not created"),
            ),
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

/// Draw build output view
fn draw_build_output(frame: &mut Frame, app: &App, area: Rect) {
    let text: Vec<Line> = app
        .build_output
        .iter()
        .map(|line| Line::from(line.as_str()))
        .collect();

    let output = Paragraph::new(text)
        .block(
            Block::default()
                .title(" Build Output ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(output, area);
}

/// Draw logs view with scrolling
fn draw_logs(frame: &mut Frame, app: &App, area: Rect) {
    let container_name = app
        .selected_container()
        .map(|c| c.name.as_str())
        .unwrap_or("Unknown");

    // Calculate visible area (accounting for borders)
    let inner_height = area.height.saturating_sub(2) as usize;
    let total_lines = app.logs.len();

    // Build visible lines with line numbers
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

    // Show scroll position in title
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

/// Draw help view
fn draw_help(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::from(""),
        Line::from(Span::styled("Navigation", Style::default().bold())),
        Line::from("  j/↓       Move down"),
        Line::from("  k/↑       Move up"),
        Line::from("  g         Go to top"),
        Line::from("  G         Go to bottom"),
        Line::from("  Enter     View details"),
        Line::from(""),
        Line::from(Span::styled("Actions", Style::default().bold())),
        Line::from("  b         Build container"),
        Line::from("  s         Start/Stop container"),
        Line::from("  u         Up (build + create + start)"),
        Line::from("  d         Delete container"),
        Line::from("  r         Refresh list"),
        Line::from("  l         View logs"),
        Line::from(""),
        Line::from(Span::styled("General", Style::default().bold())),
        Line::from("  ?         Show this help"),
        Line::from("  q/Esc     Quit / Go back"),
        Line::from(""),
    ];

    let help = Paragraph::new(text)
        .block(Block::default().title(" Help ").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(help, area);
}

/// Draw confirmation dialog
fn draw_confirm_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let message = match &app.confirm_action {
        Some(ConfirmAction::Delete(id)) => {
            let name = app
                .containers
                .iter()
                .find(|c| &c.id == id)
                .map(|c| c.name.as_str())
                .unwrap_or(id);
            format!("Delete container '{}'?", name)
        }
        Some(ConfirmAction::Stop(id)) => {
            let name = app
                .containers
                .iter()
                .find(|c| &c.id == id)
                .map(|c| c.name.as_str())
                .unwrap_or(id);
            format!("Stop container '{}'?", name)
        }
        None => return,
    };

    // Center the dialog
    let dialog_width = 40;
    let dialog_height = 5;
    let dialog_area = Rect {
        x: (area.width.saturating_sub(dialog_width)) / 2,
        y: (area.height.saturating_sub(dialog_height)) / 2,
        width: dialog_width.min(area.width),
        height: dialog_height.min(area.height),
    };

    frame.render_widget(Clear, dialog_area);

    let dialog = Paragraph::new(vec![
        Line::from(""),
        Line::from(message),
        Line::from(""),
        Line::from(Span::styled(
            "[y]es  [n]o",
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
