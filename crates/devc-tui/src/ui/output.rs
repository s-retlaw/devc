use super::*;

pub(super) fn draw_build_output(frame: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn draw_logs(frame: &mut Frame, app: &App, area: Rect) {
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
