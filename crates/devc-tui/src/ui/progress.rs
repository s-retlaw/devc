use super::spinner;
use super::*;

pub(super) fn draw_disconnection_warning(frame: &mut Frame, app: &App, area: Rect) {
    let message = app
        .connection_error
        .as_deref()
        .unwrap_or("Not connected to container provider");

    let warning = Paragraph::new(Line::from(vec![
        Span::styled(" ⚠ ", Style::default().fg(Color::Yellow).bold()),
        Span::styled("DISCONNECTED: ", Style::default().fg(Color::Yellow).bold()),
        Span::styled(message, Style::default().fg(Color::White)),
        Span::styled(
            " - Go to Providers tab and press 'c' to retry connection",
            Style::default().fg(Color::Gray),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(Color::Rgb(60, 40, 0))),
    );

    frame.render_widget(warning, area);
}

pub(super) fn draw_install_progress(frame: &mut Frame, app: &App, area: Rect) {
    let spinner = spinner::frame(app.spinner_frame);

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
pub(super) fn draw_operation_progress(frame: &mut Frame, app: &App, area: Rect) {
    let spinner = spinner::frame(app.spinner_frame);

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
