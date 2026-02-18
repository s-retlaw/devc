//! Demo mode for TUI without container runtime

use crate::event::{Event, EventHandler};
use crate::AppResult;
use crossterm::event::{KeyCode, KeyModifiers};
use devc_core::{ContainerState, DevcContainerStatus};
use devc_provider::{DevcontainerSource, ProviderType};
use ratatui::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

/// Demo application state (no container runtime needed)
pub struct DemoApp {
    pub view: crate::app::View,
    pub containers: Vec<ContainerState>,
    pub selected: usize,
    pub build_output: Vec<String>,
    pub logs: Vec<String>,
    pub logs_scroll: usize,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub confirm_action: Option<crate::app::ConfirmAction>,
    pub loading: bool,
    pub provider_type: ProviderType,
}

impl Default for DemoApp {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoApp {
    /// Create a demo app with sample data
    pub fn new() -> Self {
        let now = chrono::Utc::now();

        let containers = vec![
            ContainerState {
                id: "abc123def456".to_string(),
                name: "my-rust-project".to_string(),
                provider: ProviderType::Docker,
                config_path: PathBuf::from(
                    "/home/user/projects/rust-app/.devcontainer/devcontainer.json",
                ),
                image_id: Some("sha256:abc123...".to_string()),
                container_id: Some("container123".to_string()),
                status: DevcContainerStatus::Running,
                created_at: now - chrono::Duration::days(5),
                last_used: now - chrono::Duration::hours(2),
                workspace_path: PathBuf::from("/home/user/projects/rust-app"),
                metadata: Default::default(),
                compose_project: None,
                compose_service: None,
                source: DevcontainerSource::Devc,
            },
            ContainerState {
                id: "def456ghi789".to_string(),
                name: "python-api".to_string(),
                provider: ProviderType::Docker,
                config_path: PathBuf::from(
                    "/home/user/projects/api/.devcontainer/devcontainer.json",
                ),
                image_id: Some("sha256:def456...".to_string()),
                container_id: Some("container456".to_string()),
                status: DevcContainerStatus::Stopped,
                created_at: now - chrono::Duration::days(10),
                last_used: now - chrono::Duration::days(1),
                workspace_path: PathBuf::from("/home/user/projects/api"),
                metadata: Default::default(),
                compose_project: None,
                compose_service: None,
                source: DevcontainerSource::VsCode,
            },
            ContainerState {
                id: "ghi789jkl012".to_string(),
                name: "frontend-app".to_string(),
                provider: ProviderType::Docker,
                config_path: PathBuf::from(
                    "/home/user/projects/frontend/.devcontainer/devcontainer.json",
                ),
                image_id: None,
                container_id: None,
                status: DevcContainerStatus::Building,
                created_at: now - chrono::Duration::minutes(5),
                last_used: now,
                workspace_path: PathBuf::from("/home/user/projects/frontend"),
                metadata: Default::default(),
                compose_project: None,
                compose_service: None,
                source: DevcontainerSource::Devc,
            },
            ContainerState {
                id: "jkl012mno345".to_string(),
                name: "database-dev".to_string(),
                provider: ProviderType::Podman,
                config_path: PathBuf::from(
                    "/home/user/projects/db/.devcontainer/devcontainer.json",
                ),
                image_id: Some("sha256:jkl012...".to_string()),
                container_id: None,
                status: DevcContainerStatus::Built,
                created_at: now - chrono::Duration::days(3),
                last_used: now - chrono::Duration::hours(12),
                workspace_path: PathBuf::from("/home/user/projects/db"),
                metadata: Default::default(),
                compose_project: None,
                compose_service: None,
                source: DevcontainerSource::Other,
            },
        ];

        // Sample log data for demo
        let logs: Vec<String> = (1..=50)
            .map(|i| format!("[2024-01-15 10:00:{:02}] Sample log line {}: Container operation in progress...", i % 60, i))
            .collect();

        Self {
            view: crate::app::View::Main,
            containers,
            selected: 0,
            build_output: Vec::new(),
            logs,
            logs_scroll: 0,
            status_message: Some("Demo mode - no container runtime".to_string()),
            should_quit: false,
            confirm_action: None,
            loading: false,
            provider_type: ProviderType::Docker,
        }
    }

    /// Run the demo application
    pub async fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> AppResult<()> {
        let mut events = EventHandler::new(Duration::from_millis(250));

        while !self.should_quit {
            terminal.draw(|frame| self.draw(frame))?;

            if let Some(event) = events.next().await {
                self.handle_event(event);
            }
        }

        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        // Create a fake App-like interface for the UI
        let area = frame.size();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(area);

        self.draw_header(frame, chunks[0]);

        match self.view {
            crate::app::View::Main => self.draw_dashboard(frame, chunks[1]),
            crate::app::View::ContainerDetail => self.draw_detail(frame, chunks[1]),
            crate::app::View::ProviderDetail => self.draw_dashboard(frame, chunks[1]), // Not implemented in demo
            crate::app::View::Help => self.draw_help(frame, chunks[1]),
            crate::app::View::BuildOutput => self.draw_build(frame, chunks[1]),
            crate::app::View::Logs => self.draw_logs(frame, chunks[1]),
            crate::app::View::Ports => self.draw_dashboard(frame, chunks[1]), // Not implemented in demo
            crate::app::View::Shell => self.draw_dashboard(frame, chunks[1]), // Shell mode handled differently
            crate::app::View::DiscoverDetail => self.draw_dashboard(frame, chunks[1]), // Not implemented in demo
            crate::app::View::AgentDiagnostics => self.draw_dashboard(frame, chunks[1]), // Not implemented in demo
            crate::app::View::Confirm => {
                self.draw_dashboard(frame, chunks[1]);
                self.draw_confirm(frame, area);
            }
        }

        self.draw_footer(frame, chunks[2]);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph};

        let title = format!(
            " devc - Dev Container Manager  [{}]  [DEMO MODE] ",
            self.provider_type
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

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph};

        let help = match self.view {
            crate::app::View::Main => "[j/k] Navigate  [Enter] Details  [?] Help  [q] Quit",
            crate::app::View::ContainerDetail => "[l]ogs  [q] Back  [?] Help",
            crate::app::View::Logs => "[j/k] Scroll  [g/G] Top/Bottom  [C-d/C-u] Page  [q] Back",
            crate::app::View::Help => "Press any key to close",
            _ => "[q] Back",
        };

        let status = self.status_message.as_deref().unwrap_or("");
        let text = if status.is_empty() {
            help.to_string()
        } else {
            format!("{} | {}", status, help)
        };

        let footer = Paragraph::new(text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(footer, area);
    }

    fn draw_dashboard(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, List, ListItem};

        let items: Vec<ListItem> = self
            .containers
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let symbol = match c.status {
                    DevcContainerStatus::Available => "◌",
                    DevcContainerStatus::Running => "●",
                    DevcContainerStatus::Stopped => "○",
                    DevcContainerStatus::Building => "◐",
                    DevcContainerStatus::Built => "◑",
                    DevcContainerStatus::Created => "◔",
                    DevcContainerStatus::Failed => "✗",
                    DevcContainerStatus::Configured => "◯",
                };

                let color = match c.status {
                    DevcContainerStatus::Running => Color::Green,
                    DevcContainerStatus::Building => Color::Yellow,
                    DevcContainerStatus::Failed => Color::Red,
                    _ => Color::DarkGray,
                };

                let style = if i == self.selected {
                    Style::default().bg(Color::DarkGray).fg(Color::White)
                } else {
                    Style::default()
                };

                let line = Line::from(vec![
                    Span::styled(format!(" {} ", symbol), Style::default().fg(color)),
                    Span::styled(format!("{:<20}", c.name), style.bold()),
                    Span::styled(format!("{:<12}", c.status), style.fg(color)),
                    Span::styled(format!("{:<10}", c.provider), style.fg(Color::DarkGray)),
                ]);

                ListItem::new(line).style(style)
            })
            .collect();

        let list =
            List::new(items).block(Block::default().title(" Containers ").borders(Borders::ALL));
        frame.render_widget(list, area);
    }

    fn draw_detail(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

        if let Some(c) = self.containers.get(self.selected) {
            let text = vec![
                Line::from(vec![
                    Span::raw("Name:        "),
                    Span::styled(&c.name, Style::default().bold()),
                ]),
                Line::from(vec![
                    Span::raw("Status:      "),
                    Span::raw(c.status.to_string()),
                ]),
                Line::from(vec![
                    Span::raw("Provider:    "),
                    Span::raw(c.provider.to_string()),
                ]),
                Line::from(vec![Span::raw("ID:          "), Span::raw(&c.id)]),
                Line::from(""),
                Line::from(vec![
                    Span::raw("Workspace:   "),
                    Span::raw(c.workspace_path.to_string_lossy().to_string()),
                ]),
            ];

            let detail = Paragraph::new(text)
                .block(
                    Block::default()
                        .title(format!(" {} ", c.name))
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true });
            frame.render_widget(detail, area);
        }
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

        let text = vec![
            Line::from(""),
            Line::from(Span::styled("Navigation", Style::default().bold())),
            Line::from("  j/↓       Move down"),
            Line::from("  k/↑       Move up"),
            Line::from("  Enter     View details"),
            Line::from(""),
            Line::from(Span::styled(
                "Actions (disabled in demo)",
                Style::default().bold(),
            )),
            Line::from("  s         Start/Stop"),
            Line::from("  u         Up (full lifecycle)"),
            Line::from("  d         Delete"),
            Line::from(""),
            Line::from(Span::styled("General", Style::default().bold())),
            Line::from("  ?         Show this help"),
            Line::from("  q/Esc     Quit / Go back"),
        ];

        let help = Paragraph::new(text)
            .block(Block::default().title(" Help ").borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        frame.render_widget(help, area);
    }

    fn draw_build(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph};

        let text: Vec<Line> = self
            .build_output
            .iter()
            .map(|s| Line::from(s.as_str()))
            .collect();
        let output = Paragraph::new(text).block(
            Block::default()
                .title(" Build Output ")
                .borders(Borders::ALL),
        );
        frame.render_widget(output, area);
    }

    fn draw_logs(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph};

        let container_name = self
            .selected_container()
            .map(|c| c.name.as_str())
            .unwrap_or("Unknown");

        let inner_height = area.height.saturating_sub(2) as usize;
        let total_lines = self.logs.len();

        let text: Vec<Line> = self
            .logs
            .iter()
            .enumerate()
            .skip(self.logs_scroll)
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
                ((self.logs_scroll + inner_height).min(total_lines) * 100) / total_lines
            };
            format!(
                " Logs: {} [{}/{}] {}% ",
                container_name,
                self.logs_scroll + 1,
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

    fn draw_confirm(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Clear, Paragraph};

        let dialog_area = Rect {
            x: (area.width.saturating_sub(40)) / 2,
            y: (area.height.saturating_sub(5)) / 2,
            width: 40.min(area.width),
            height: 5.min(area.height),
        };

        frame.render_widget(Clear, dialog_area);

        let dialog = Paragraph::new(vec![
            Line::from(""),
            Line::from("Action disabled in demo mode"),
            Line::from(""),
            Line::from(Span::styled(
                "[any key] Close",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(" Demo ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );

        frame.render_widget(dialog, dialog_area);
    }

    fn handle_event(&mut self, event: Event) {
        if let Event::Key(key) = event {
            self.handle_key(key.code, key.modifiers);
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if self.view == crate::app::View::Confirm {
            self.view = crate::app::View::Main;
            return;
        }

        // Handle Logs view navigation separately
        if self.view == crate::app::View::Logs {
            let page_size = 20;
            match code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.view = crate::app::View::ContainerDetail;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if self.logs_scroll < self.logs.len().saturating_sub(1) {
                        self.logs_scroll += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.logs_scroll = self.logs_scroll.saturating_sub(1);
                }
                KeyCode::Char('g') => {
                    self.logs_scroll = 0;
                }
                KeyCode::Char('G') => {
                    self.logs_scroll = self.logs.len().saturating_sub(1);
                }
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.logs_scroll =
                        (self.logs_scroll + page_size / 2).min(self.logs.len().saturating_sub(1));
                }
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.logs_scroll = self.logs_scroll.saturating_sub(page_size / 2);
                }
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.view == crate::app::View::Main {
                    self.should_quit = true;
                } else {
                    self.view = crate::app::View::Main;
                }
            }
            KeyCode::Char('?') => self.view = crate::app::View::Help,
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.containers.is_empty() {
                    self.selected = (self.selected + 1) % self.containers.len();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.containers.is_empty() {
                    self.selected = self
                        .selected
                        .checked_sub(1)
                        .unwrap_or(self.containers.len() - 1);
                }
            }
            KeyCode::Enter => {
                if !self.containers.is_empty() {
                    self.view = crate::app::View::ContainerDetail;
                }
            }
            KeyCode::Char('l') if self.view == crate::app::View::ContainerDetail => {
                self.logs_scroll = self.logs.len().saturating_sub(1);
                self.view = crate::app::View::Logs;
            }
            KeyCode::Char('s') | KeyCode::Char('u') | KeyCode::Char('d') => {
                self.view = crate::app::View::Confirm;
            }
            _ => {}
        }
    }

    pub fn selected_container(&self) -> Option<&ContainerState> {
        self.containers.get(self.selected)
    }
}
