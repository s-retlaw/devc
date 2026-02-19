use super::*;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitInfo {
    repo_display: String,
    branch: String,
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
}

fn resolve_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if !dot_git.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(dot_git).ok()?;
    let gitdir_raw = content
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))
        .map(str::trim)?;
    if gitdir_raw.is_empty() {
        return None;
    }

    let gitdir_path = Path::new(gitdir_raw);
    let resolved = if gitdir_path.is_absolute() {
        gitdir_path.to_path_buf()
    } else {
        repo_root.join(gitdir_path)
    };

    resolved.exists().then_some(resolved)
}

fn read_head_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head_line = head.lines().next()?.trim();
    let branch = head_line.strip_prefix("ref: refs/heads/")?.trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

fn read_origin_remote(git_dir: &Path) -> Option<String> {
    let config = std::fs::read_to_string(git_dir.join("config")).ok()?;
    let mut in_origin = false;

    for raw_line in config.lines() {
        let line = raw_line.trim();

        if line.starts_with('[') && line.ends_with(']') {
            in_origin = line == "[remote \"origin\"]";
            continue;
        }

        if !in_origin {
            continue;
        }

        let (key, value) = line.split_once('=')?;
        if key.trim() == "url" {
            let url = value.trim();
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }

    None
}

fn normalize_remote_display(host: &str, path: &str) -> Option<String> {
    let host = host.trim();
    let mut path = path.trim().trim_matches('/').to_string();
    if let Some(stripped) = path.strip_suffix(".git") {
        path = stripped.to_string();
    }
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host, path))
}

fn remote_display_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return normalize_remote_display(host, path);
    }

    if let Some((scheme, rest)) = url.split_once("://") {
        if !scheme.is_empty() {
            let (authority, path) = rest.split_once('/')?;
            let host = authority
                .rsplit_once('@')
                .map(|(_, h)| h)
                .unwrap_or(authority);
            return normalize_remote_display(host, path);
        }
    }

    if let Some((host, path)) = url.split_once(':') {
        if !host.contains('/') && !host.contains('\\') && path.contains('/') {
            return normalize_remote_display(host, path);
        }
    }

    None
}

fn git_info_for_workspace(workspace: &Path) -> Option<GitInfo> {
    let repo_root = find_git_root(workspace)?;
    let git_dir = resolve_git_dir(&repo_root)?;
    let branch = read_head_branch(&git_dir)?;
    let repo_name = repo_root.file_name()?.to_string_lossy().to_string();
    if repo_name.is_empty() {
        return None;
    }

    let repo_display = read_origin_remote(&git_dir)
        .and_then(|url| remote_display_from_url(&url))
        .unwrap_or(repo_name);

    Some(GitInfo {
        repo_display,
        branch,
    })
}

pub(super) fn draw_provider_detail(frame: &mut Frame, app: &App, area: Rect) {
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
        Span::styled(
            format!("{:<16}", socket_label),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(socket_value, socket_style),
        if !detail_state.editing {
            Span::styled(
                "  [e] to edit",
                Style::default().fg(Color::DarkGray).italic(),
            )
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
            let error_msg = detail_state
                .connection_error
                .as_deref()
                .unwrap_or("Unknown error");
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
                    Span::styled(
                        "  [t] to test",
                        Style::default().fg(Color::DarkGray).italic(),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled("Connection:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled("○ Not tested", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        "  [t] to test",
                        Style::default().fg(Color::DarkGray).italic(),
                    ),
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
                Span::styled(
                    "sudo systemctl start docker",
                    Style::default().fg(Color::White),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  • Default socket: ", Style::default().fg(Color::DarkGray)),
                Span::styled("/var/run/docker.sock", Style::default().fg(Color::White)),
            ]));
        }
        devc_provider::ProviderType::Podman => {
            lines.push(Line::from(vec![
                Span::styled("  • Start Podman: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "systemctl --user start podman.socket",
                    Style::default().fg(Color::White),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  • Default socket: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "$XDG_RUNTIME_DIR/podman/podman.sock",
                    Style::default().fg(Color::White),
                ),
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

/// Build the info text lines for the container detail view
pub(super) fn build_detail_text(
    container: &devc_core::ContainerState,
    details: Option<&devc_provider::ContainerDetails>,
) -> Vec<Line<'static>> {
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

    let mut runtime_lines = vec![
        Line::from(Span::styled(
            "─── Runtime ───",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::raw("Image ID:    "),
            Span::raw(
                container
                    .image_id
                    .as_deref()
                    .unwrap_or("Not built")
                    .to_string(),
            ),
        ]),
    ];
    if let Some(d) = details {
        runtime_lines.push(Line::from(vec![
            Span::raw("Runtime Name: "),
            Span::raw(d.name.clone()),
        ]));
    }
    runtime_lines.push(Line::from(vec![
        Span::raw("Container ID: "),
        Span::raw(
            container
                .container_id
                .as_deref()
                .unwrap_or("Not created")
                .to_string(),
        ),
    ]));
    if let Some(code) = details.and_then(|d| d.exit_code) {
        let color = if code == 0 { Color::Green } else { Color::Red };
        runtime_lines.push(Line::from(vec![
            Span::raw("Exit Code:   "),
            Span::styled(code.to_string(), Style::default().fg(color)),
        ]));
    }

    let mut lines = vec![
        Line::from(Span::styled(
            "─── Identity ───",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::raw("Name:        "),
            Span::styled(container.name.clone(), Style::default().bold()),
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
            Span::raw("Source:      "),
            Span::styled(
                format!("{:?}", container.source),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(vec![
            Span::raw("ID:          "),
            Span::styled(container.id.clone(), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "─── Workspace ───",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::raw("Workspace:   "),
            Span::raw(container.workspace_path.to_string_lossy().into_owned()),
        ]),
        Line::from(vec![
            Span::raw("Config:      "),
            Span::raw(container.config_path.to_string_lossy().into_owned()),
        ]),
        Line::from(""),
    ];
    lines.extend(runtime_lines);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "─── Timestamps ───",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(vec![
        Span::raw("Created:     "),
        Span::raw(container.created_at.format("%Y-%m-%d %H:%M:%S").to_string()),
    ]));
    lines.push(Line::from(vec![
        Span::raw("Last used:   "),
        Span::raw(container.last_used.format("%Y-%m-%d %H:%M:%S").to_string()),
    ]));

    if let Some(git) = git_info_for_workspace(&container.workspace_path) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Git ───",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(vec![
            Span::raw("Repo:        "),
            Span::raw(git.repo_display),
        ]));
        lines.push(Line::from(vec![
            Span::raw("Branch:      "),
            Span::raw(git.branch),
        ]));
    }

    // Add inspect-based sections when available
    if let Some(details) = details {
        // Ports
        if !details.ports.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── Ports ───",
                Style::default().fg(Color::DarkGray),
            )));
            for p in &details.ports {
                let host = p
                    .host_port
                    .map(|hp| hp.to_string())
                    .unwrap_or_else(|| "-".to_string());
                lines.push(Line::from(format!(
                    "  {}:{} → {}",
                    host, p.container_port, p.protocol,
                )));
            }
        }

        // Mounts (all types)
        if !details.mounts.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── Mounts ───",
                Style::default().fg(Color::DarkGray),
            )));
            for m in &details.mounts {
                let ro = if m.read_only { " (ro)" } else { "" };
                lines.push(Line::from(format!(
                    "  [{}] {} → {}{}",
                    m.mount_type, m.source, m.destination, ro,
                )));
            }
        }

        // Networks
        let has_network = details.network_settings.ip_address.is_some()
            || details.network_settings.gateway.is_some()
            || !details.network_settings.networks.is_empty();
        if has_network {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── Network ───",
                Style::default().fg(Color::DarkGray),
            )));
            if let Some(ip) = &details.network_settings.ip_address {
                lines.push(Line::from(vec![
                    Span::raw("IP:          "),
                    Span::raw(ip.clone()),
                ]));
            }
            if let Some(gw) = &details.network_settings.gateway {
                lines.push(Line::from(vec![
                    Span::raw("Gateway:     "),
                    Span::raw(gw.clone()),
                ]));
            }
            let mut net_names: Vec<_> = details.network_settings.networks.keys().collect();
            net_names.sort();
            for net_name in net_names {
                let net_info = &details.network_settings.networks[net_name];
                let mut parts = vec![Span::raw(format!("  {}:", net_name))];
                if let Some(ip) = &net_info.ip_address {
                    parts.push(Span::raw(format!(" {}", ip)));
                }
                if let Some(gw) = &net_info.gateway {
                    parts.push(Span::styled(
                        format!(" (gw {})", gw),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                lines.push(Line::from(parts));
            }
        }

        // Labels
        if !details.labels.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── Labels ───",
                Style::default().fg(Color::DarkGray),
            )));

            let well_known = [
                "devcontainer.local_folder",
                "devcontainer.config_file",
                "devc.managed",
                "devc.project",
                "devc.workspace",
                "com.docker.compose.service",
                "com.docker.compose.project",
            ];
            for key in well_known {
                if let Some(val) = details.labels.get(key) {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}: ", key), Style::default().fg(Color::Cyan)),
                        Span::raw(val.clone()),
                    ]));
                }
            }

            let mut remaining: Vec<_> = details
                .labels
                .iter()
                .filter(|(k, _)| {
                    !well_known.contains(&k.as_str()) && k.as_str() != "devcontainer.metadata"
                })
                .collect();
            remaining.sort_by_key(|(k, _)| (*k).clone());
            for (key, val) in remaining {
                lines.push(Line::from(format!("  {}: {}", key, val)));
            }
        }

        // Environment
        if !details.env.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── Environment ───",
                Style::default().fg(Color::DarkGray),
            )));

            let skip_prefixes = [
                "PATH=",
                "HOME=",
                "HOSTNAME=",
                "TERM=",
                "LANG=",
                "SHELL=",
                "USER=",
                "SHLVL=",
                "PWD=",
                "OLDPWD=",
                "LC_",
                "LESSOPEN=",
                "LESSCLOSE=",
                "LS_COLORS=",
                "_=",
            ];
            let mut env_sorted = details.env.clone();
            env_sorted.sort();
            for var in &env_sorted {
                if !skip_prefixes.iter().any(|p| var.starts_with(p)) {
                    lines.push(Line::from(format!("  {}", var)));
                }
            }
        }
    }

    lines
}

/// Draw the container detail view
pub(super) fn draw_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let container = match app.selected_container() {
        Some(c) => c.clone(),
        None => return,
    };

    let is_compose = container.compose_project.is_some();
    let text = build_detail_text(&container, app.container_detail.as_ref());

    if is_compose {
        // For compose containers, render outer block then split into info + services
        let outer_block = Block::default()
            .title(format!(" {} ", container.name))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        let inner_area = outer_block.inner(area);
        frame.render_widget(outer_block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(12), // Info paragraph
                Constraint::Min(6),  // Services table
            ])
            .split(inner_area);

        let info = Paragraph::new(text.clone())
            .wrap(Wrap { trim: true })
            .scroll((app.container_detail_scroll as u16, 0));
        frame.render_widget(info, chunks[0]);
        let info_inner_height = chunks[0].height.saturating_sub(2) as usize;
        if text.len() > info_inner_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"));
            let mut scrollbar_state =
                ScrollbarState::new(text.len().saturating_sub(info_inner_height))
                    .position(app.container_detail_scroll);
            let scrollbar_area = Rect {
                x: chunks[0].x + chunks[0].width.saturating_sub(1),
                y: chunks[0].y + 1,
                width: 1,
                height: chunks[0].height.saturating_sub(2),
            };
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }

        draw_compose_services(frame, app, &container, chunks[1]);
    } else {
        // Non-compose: scrollable Paragraph
        let detail = Paragraph::new(text.clone())
            .block(
                Block::default()
                    .title(format!(" {} ", container.name))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: true })
            .scroll((app.container_detail_scroll as u16, 0));

        frame.render_widget(detail, area);
        let inner_height = area.height.saturating_sub(2) as usize;
        if text.len() > inner_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"));
            let mut scrollbar_state = ScrollbarState::new(text.len().saturating_sub(inner_height))
                .position(app.container_detail_scroll);
            let scrollbar_area = Rect {
                x: area.x + area.width.saturating_sub(1),
                y: area.y + 1,
                width: 1,
                height: area.height.saturating_sub(2),
            };
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }
    }
}

/// Build detail text lines from a ContainerDetails (discovered container inspect)
pub(super) fn build_discover_detail_text(
    details: &devc_provider::ContainerDetails,
    discovered: &devc_provider::DiscoveredContainer,
) -> Vec<Line<'static>> {
    use devc_provider::ContainerStatus;

    let status_color = match details.status {
        ContainerStatus::Running => Color::Green,
        ContainerStatus::Exited | ContainerStatus::Dead => Color::Red,
        ContainerStatus::Paused => Color::Yellow,
        ContainerStatus::Created => Color::Cyan,
        _ => Color::DarkGray,
    };

    let format_ts = |ts: i64| -> String {
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "N/A".to_string())
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "─── Identity ───",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::raw("Name:        "),
            Span::styled(details.name.clone(), Style::default().bold()),
        ]),
        Line::from(vec![
            Span::raw("ID:          "),
            Span::styled(
                details.id.0.chars().take(12).collect::<String>(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::raw("Image:       "),
            Span::raw(details.image.clone()),
        ]),
        Line::from(vec![
            Span::raw("Image ID:    "),
            Span::styled(
                details.image_id.chars().take(19).collect::<String>(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::raw("Provider:    "),
            Span::raw(discovered.provider.to_string()),
        ]),
        Line::from(vec![
            Span::raw("Source:      "),
            Span::styled(
                format!("{:?}", discovered.source),
                Style::default().fg(Color::Cyan),
            ),
        ]),
    ];
    if let Some(ws) = &discovered.workspace_path {
        lines.push(Line::from(vec![
            Span::raw("Workspace:   "),
            Span::raw(ws.clone()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "─── Status ───",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(vec![
        Span::raw("Status:      "),
        Span::styled(
            format!("{:?}", details.status),
            Style::default().fg(status_color).bold(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw("Created:     "),
        Span::raw(format_ts(details.created)),
    ]));

    if let Some(ts) = details.started_at {
        lines.push(Line::from(vec![
            Span::raw("Started:     "),
            Span::raw(format_ts(ts)),
        ]));
    }
    if let Some(ts) = details.finished_at {
        lines.push(Line::from(vec![
            Span::raw("Finished:    "),
            Span::raw(format_ts(ts)),
        ]));
    }
    if let Some(code) = details.exit_code {
        let color = if code == 0 { Color::Green } else { Color::Red };
        lines.push(Line::from(vec![
            Span::raw("Exit Code:   "),
            Span::styled(code.to_string(), Style::default().fg(color)),
        ]));
    }

    // Ports
    if !details.ports.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Ports ───",
            Style::default().fg(Color::DarkGray),
        )));
        for p in &details.ports {
            let host = p
                .host_port
                .map(|hp| hp.to_string())
                .unwrap_or_else(|| "-".to_string());
            lines.push(Line::from(format!(
                "  {}:{} → {}",
                host, p.container_port, p.protocol,
            )));
        }
    }

    // Mounts (all types)
    if !details.mounts.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Mounts ───",
            Style::default().fg(Color::DarkGray),
        )));
        for m in &details.mounts {
            let ro = if m.read_only { " (ro)" } else { "" };
            lines.push(Line::from(format!(
                "  [{}] {} → {}{}",
                m.mount_type, m.source, m.destination, ro,
            )));
        }
    }

    // Networks
    let has_network = details.network_settings.ip_address.is_some()
        || details.network_settings.gateway.is_some()
        || !details.network_settings.networks.is_empty();
    if has_network {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Network ───",
            Style::default().fg(Color::DarkGray),
        )));
        if let Some(ip) = &details.network_settings.ip_address {
            lines.push(Line::from(vec![
                Span::raw("IP:          "),
                Span::raw(ip.clone()),
            ]));
        }
        if let Some(gw) = &details.network_settings.gateway {
            lines.push(Line::from(vec![
                Span::raw("Gateway:     "),
                Span::raw(gw.clone()),
            ]));
        }
        let mut net_names: Vec<_> = details.network_settings.networks.keys().collect();
        net_names.sort();
        for net_name in net_names {
            let net_info = &details.network_settings.networks[net_name];
            let mut parts = vec![Span::raw(format!("  {}:", net_name))];
            if let Some(ip) = &net_info.ip_address {
                parts.push(Span::raw(format!(" {}", ip)));
            }
            if let Some(gw) = &net_info.gateway {
                parts.push(Span::styled(
                    format!(" (gw {})", gw),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            lines.push(Line::from(parts));
        }
    }

    // Labels
    if !details.labels.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Labels ───",
            Style::default().fg(Color::DarkGray),
        )));

        let well_known = [
            "devcontainer.local_folder",
            "devcontainer.config_file",
            "devc.managed",
            "devc.project",
            "devc.workspace",
            "com.docker.compose.service",
            "com.docker.compose.project",
        ];
        for key in well_known {
            if let Some(val) = details.labels.get(key) {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}: ", key), Style::default().fg(Color::Cyan)),
                    Span::raw(val.clone()),
                ]));
            }
        }

        let mut remaining: Vec<_> = details
            .labels
            .iter()
            .filter(|(k, _)| {
                !well_known.contains(&k.as_str()) && k.as_str() != "devcontainer.metadata"
            })
            .collect();
        remaining.sort_by_key(|(k, _)| (*k).clone());
        for (key, val) in remaining {
            lines.push(Line::from(format!("  {}: {}", key, val)));
        }
    }

    // Environment
    if !details.env.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "─── Environment ───",
            Style::default().fg(Color::DarkGray),
        )));

        let skip_prefixes = [
            "PATH=",
            "HOME=",
            "HOSTNAME=",
            "TERM=",
            "LANG=",
            "SHELL=",
            "USER=",
            "SHLVL=",
            "PWD=",
            "OLDPWD=",
            "LC_",
            "LESSOPEN=",
            "LESSCLOSE=",
            "LS_COLORS=",
            "_=",
        ];
        let mut env_sorted = details.env.clone();
        env_sorted.sort();
        for var in &env_sorted {
            if !skip_prefixes.iter().any(|p| var.starts_with(p)) {
                lines.push(Line::from(format!("  {}", var)));
            }
        }
    }

    lines
}

/// Draw the discover detail popup
pub(super) fn draw_discover_detail(frame: &mut Frame, app: &App, area: Rect) {
    let discovered = app.discovered_containers.get(app.selected_discovered);
    let name = discovered.map(|c| c.name.as_str()).unwrap_or("Unknown");
    let lines = match (&app.discover_detail, discovered) {
        (Some(details), Some(disc)) => build_discover_detail_text(details, disc),
        _ => vec![Line::from("Loading...")],
    };
    let detail = Paragraph::new(lines.clone())
        .block(
            Block::default()
                .title(format!(" {} ", name))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: true })
        .scroll((app.discover_detail_scroll as u16, 0));
    frame.render_widget(detail, area);

    let inner_height = area.height.saturating_sub(2) as usize;
    if app
        .discover_detail
        .as_ref()
        .is_some_and(|_| lines.len() > inner_height)
    {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));
        let mut scrollbar_state = ScrollbarState::new(lines.len().saturating_sub(inner_height))
            .position(app.discover_detail_scroll);
        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

/// Draw the compose services table within the detail popup
pub(super) fn draw_compose_services(
    frame: &mut Frame,
    app: &mut App,
    container: &devc_core::ContainerState,
    area: Rect,
) {
    let services = app.compose_state.services.get(&container.id);

    if app.compose_state.services_loading && services.is_none() {
        let loading = Paragraph::new("Loading services...")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .title(" Compose Services ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        frame.render_widget(loading, area);
        return;
    }

    let services = match services {
        Some(s) if !s.is_empty() => s,
        _ => {
            let empty = Paragraph::new("No services found")
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .title(" Compose Services ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
            frame.render_widget(empty, area);
            return;
        }
    };

    let primary_service = container.compose_service.as_deref();

    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("Service"),
        Cell::from("Status"),
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(0);

    let rows: Vec<Row> = services
        .iter()
        .map(|svc| {
            let is_primary = primary_service == Some(svc.service_name.as_str());
            let status_icon = match svc.status {
                devc_provider::ContainerStatus::Running => "●",
                devc_provider::ContainerStatus::Exited => "○",
                _ => "?",
            };
            let status_color = match svc.status {
                devc_provider::ContainerStatus::Running => Color::Green,
                devc_provider::ContainerStatus::Exited => Color::DarkGray,
                _ => Color::Yellow,
            };

            let name = if is_primary {
                format!("{} (dev)", svc.service_name)
            } else {
                svc.service_name.clone()
            };

            Row::new(vec![
                Cell::from(status_icon).style(Style::default().fg(status_color)),
                Cell::from(name).style(Style::default().bold()),
                Cell::from(svc.status.to_string()).style(Style::default().fg(status_color)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(3),  // Status icon
        Constraint::Length(18), // Service name
        Constraint::Min(10),    // Status
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(" Compose Services ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.compose_state.services_table_state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_info_for_workspace_with_git_dir_head_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        let workspace = repo.join("subdir");
        let git_dir = repo.join(".git");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/a\n").unwrap();

        let info = git_info_for_workspace(&workspace).unwrap();
        assert_eq!(info.repo_display, "myrepo");
        assert_eq!(info.branch, "feature/a");
    }

    #[test]
    fn test_git_info_for_workspace_with_git_file_gitdir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let actual_git = tmp.path().join("repo.git");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&actual_git).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: ../repo.git\n").unwrap();
        std::fs::write(actual_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let info = git_info_for_workspace(&repo).unwrap();
        assert_eq!(info.repo_display, "repo");
        assert_eq!(info.branch, "main");
    }

    #[test]
    fn test_remote_display_from_url_https() {
        let display = remote_display_from_url("https://github.com/s-retlaw/devc.git").unwrap();
        assert_eq!(display, "github.com/s-retlaw/devc");
    }

    #[test]
    fn test_remote_display_from_url_ssh_scp_style() {
        let display = remote_display_from_url("git@github.com:s-retlaw/devc.git").unwrap();
        assert_eq!(display, "github.com/s-retlaw/devc");
    }

    #[test]
    fn test_remote_display_from_url_ssh_url_style() {
        let display = remote_display_from_url("ssh://git@gitlab.com/group/sub/repo.git").unwrap();
        assert_eq!(display, "gitlab.com/group/sub/repo");
    }

    #[test]
    fn test_git_info_prefers_origin_remote_display() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let git_dir = repo.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            git_dir.join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/acme/myrepo.git\n",
        )
        .unwrap();

        let info = git_info_for_workspace(&repo).unwrap();
        assert_eq!(info.repo_display, "github.com/acme/myrepo");
        assert_eq!(info.branch, "main");
    }

    #[test]
    fn test_git_info_for_workspace_returns_none_without_git() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("nogit");
        std::fs::create_dir_all(&workspace).unwrap();
        assert!(git_info_for_workspace(&workspace).is_none());
    }

    #[test]
    fn test_git_info_for_workspace_returns_none_for_detached_head() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let git_dir = repo.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            git_dir.join("HEAD"),
            "2a0f1496293197f8f4f8cbf5f18284d888fff123\n",
        )
        .unwrap();

        assert!(git_info_for_workspace(&repo).is_none());
    }
}
