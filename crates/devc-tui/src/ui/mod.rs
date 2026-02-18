//! UI rendering for the TUI application

mod containers;
mod detail;
mod dialogs;
mod header_footer;
mod output;
mod ports;
mod progress;

use crate::app::{App, ConfirmAction, ContainerOperation, DialogFocus, Tab, View};
use crate::settings::SettingsSection;
use crate::widgets::{centered_rect, DialogBuilder};
use ansi_to_tui::IntoText;
use devc_core::DevcContainerStatus;
use devc_provider::{ContainerStatus, DevcontainerSource};
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, Tabs, Wrap,
    },
};

use containers::*;
use detail::*;
use dialogs::*;
use header_footer::*;
use output::*;
use ports::*;
use progress::*;

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
            let is_compose = app
                .selected_container()
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
            let port_rows = app.port_state.detected_ports.len().max(3) as u16;
            let h = (port_rows + 7).max(12);
            let popup = popup_rect(80, 70, 56, h, content_area);
            frame.render_widget(Clear, popup);
            draw_ports(frame, app, popup);
            if app.port_state.socat_installing {
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
        View::AgentDiagnostics => {
            draw_main_content(frame, app, content_area);
            let popup = popup_rect(80, 70, 60, 18, content_area);
            frame.render_widget(Clear, popup);
            draw_agent_diagnostics(frame, app, popup);
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
